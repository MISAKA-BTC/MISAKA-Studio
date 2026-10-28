//! **The coding agent's hands: one folder the user chose, read, searched, edited and run in.**
//!
//! Everything here is relative to a `root` the person picked in the folder browser. The agent names
//! files the way a person would — `src/main.rs` — and every one of those names goes through
//! [`Workspace::resolve`] before the disk is touched, because the agent is a language model and a
//! language model will, sooner or later, write `../../.ssh/id_ed25519`. The rules:
//!
//! * **Lexically first.** `..` is resolved against the name, not the disk, and a name that climbs
//!   out of the root is refused before any syscall sees it.
//! * **Then on disk.** The target — or, for a file that does not exist yet, its nearest existing
//!   ancestor — is canonicalized and must still be inside the canonical root. That is the check a
//!   symlink cannot get past: `docs -> /etc` resolves to `/etc` and is refused, however innocent
//!   `docs/passwd` looks as text.
//! * **`.git` is read-only.** Reading it is how an agent answers "what branch is this", and the
//!   walks skip it anyway; writing, editing or deleting inside it is how a repository is corrupted
//!   by a model that meant well.
//!
//! The walks (`tree`, `files`, `search`) are ripgrep's own — the `ignore` crate — so a
//! `.gitignore` means here exactly what it means to `rg` and `git`, dotfiles such as `.github/`
//! are included, and `.git/` never is. A folder with no ignore file of its own still has a
//! `node_modules` nobody wants listed, so then the usual build and dependency directories are
//! skipped by name.
//!
//! Errors are written for the model that will read them, not for a log: "old_string matches 3
//! places in src/lib.rs; include more surrounding lines…" is a sentence a small local model can act
//! on, where `ambiguous match` is one it retries unchanged until the turn budget runs out.
//!
//! Every function here blocks on the filesystem except [`exec`], [`exec_stream`] and those in
//! [`git`]; the HTTP layer runs them on the blocking pool.

pub mod git;

use crate::{Error, Result};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// `tree`: how deep, by default and at most.
pub const TREE_DEFAULT_DEPTH: usize = 2;
pub const TREE_MAX_DEPTH: usize = 8;
/// `tree`: how many entries come back, by default and at most.
pub const TREE_DEFAULT_LIMIT: usize = 400;
pub const TREE_MAX_LIMIT: usize = 5000;
/// `files`: how many paths come back, by default and at most.
pub const FILES_DEFAULT_LIMIT: usize = 20;
pub const FILES_MAX_LIMIT: usize = 200;
/// `read`: the default byte budget for the returned range, and the most a caller may ask for.
pub const READ_DEFAULT_MAX_BYTES: usize = 256 * 1024;
pub const READ_MAX_BYTES: usize = 8 * 1024 * 1024;
/// `search`: matches returned, by default and at most.
pub const SEARCH_DEFAULT_MAX_RESULTS: usize = 100;
pub const SEARCH_MAX_RESULTS: usize = 1000;
/// `search` skips anything larger: a megabyte of one file is a lockfile, a bundle or a dataset,
/// never the line someone is looking for.
pub const SEARCH_MAX_FILE_BYTES: u64 = 1024 * 1024;
/// `write`/`edit`: the largest file they will produce.
pub const WRITE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// `exec`: the timeout, by default and at most.
pub const EXEC_DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const EXEC_MAX_TIMEOUT_SECS: u64 = 900;
/// `exec`: bytes kept per stream, half from the start and half from the end.
pub const EXEC_STREAM_CAP: usize = 100 * 1024;
/// `exec_stream`: the timeout, by default and at most. Longer than `exec`'s: this is the
/// terminal, where an `npm install` or a whole test suite is expected to take a while.
pub const EXEC_STREAM_DEFAULT_TIMEOUT_SECS: u64 = 600;
pub const EXEC_STREAM_MAX_TIMEOUT_SECS: u64 = 3600;
/// `exec_stream`: the most output it forwards, stdout and stderr together. Past it, one note
/// says so and the rest is read and dropped, so the command is never blocked on a full pipe.
pub const EXEC_STREAM_OUTPUT_CAP: usize = 8 * 1024 * 1024;

/// A walk stops looking after this many entries — a home directory opened as a workspace must
/// still answer, and say it stopped, rather than stat a million files.
const WALK_CAP: usize = 50_000;
/// `search` stops after scanning this many files, for the same reason.
const SEARCH_SCAN_CAP: usize = 50_000;
/// A file whose first 8 KiB hold a NUL byte is binary — git's own test, and ripgrep's.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// `write` returns the old content only up to this size; past it `previous` is `null`.
const PREVIOUS_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// A search hit's line, cut to this many characters around the match.
const SEARCH_LINE_CHARS: usize = 300;

/// Version-control metadata: never listed, never searched.
const VCS_DIRS: &[&str] = &[".git", ".hg", ".svn", ".jj"];
/// Build output, dependencies and caches — skipped by name in a folder that has no ignore file to
/// say so itself. With a `.gitignore`, the project's own word wins, and a `build/` it tracks is
/// listed like any other directory.
const HEAVY_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".gradle",
];

fn bad(message: impl Into<String>) -> Error {
    Error::bad_request(message)
}

/// A path for a person to read: no `\\?\` verbatim prefix on Windows, which `canonicalize` adds
/// and nobody types.
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    #[cfg(windows)]
    {
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return rest.to_string();
        }
    }
    text
}

/// `a/b/c`, whatever the platform's separator; `.` for the root itself.
fn slash_path(rel: &Path) -> String {
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| p.is_absolute())
}

/// `~` and `~/…`, for the two places a person types a path: the root and the folder browser.
fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// `..` resolved against the name alone. `None` when it climbs above where it started, or when
/// the "relative" path turns out to have a root.
fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// The same for an absolute path. `..` at the filesystem root stays there, as the OS would.
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// **Where a path really is**: canonical when it exists; otherwise its nearest existing ancestor,
/// canonical, with the missing components after it. The second form is what lets a file that is
/// about to be created be checked against the root through whatever symlinks lead to its folder.
///
/// Returns `(path, exists)`. A dangling symlink counts as missing — what it names is not there.
fn real_path(lexical: &Path) -> std::io::Result<(PathBuf, bool)> {
    let mut existing = lexical.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(canonical) => {
                let exists = missing.is_empty();
                let mut out = canonical;
                for part in missing.iter().rev() {
                    out.push(part);
                }
                return Ok((out, exists));
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
                match (existing.file_name().map(|n| n.to_os_string()), existing.parent().map(Path::to_path_buf)) {
                    (Some(name), Some(parent)) => {
                        missing.push(name);
                        existing = parent;
                    }
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// A folder the user opened.
#[derive(Clone, Debug)]
pub struct Workspace {
    /// Canonical — every containment check compares against this.
    root: PathBuf,
}

/// A path inside a workspace, checked.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// Relative to the root, `/`-separated — the name as the agent should keep using it. `.` for
    /// the root itself.
    pub rel: String,
    /// Where it is on disk, symlinks resolved. For a path that does not exist yet: the canonical
    /// nearest existing ancestor joined with the rest.
    pub real: PathBuf,
    pub exists: bool,
}

impl Workspace {
    /// Open `root`: an absolute path to an existing directory (`~` is expanded).
    pub fn open(root: &str) -> Result<Workspace> {
        let given = root.trim();
        if given.is_empty() {
            return Err(bad("root is required: the absolute path of the folder that was opened"));
        }
        let path = expand_home(given);
        if !path.is_absolute() {
            return Err(bad(format!("root must be an absolute path, got {given:?}")));
        }
        let root = std::fs::canonicalize(&path).map_err(|e| bad(format!("root {}: {e}", path.display())))?;
        if !root.is_dir() {
            return Err(bad(format!("root {} is not a directory", display_path(&root))));
        }
        Ok(Workspace { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn escapes(&self, path: &str, hint: Option<String>) -> Error {
        let mut message =
            format!("path escapes the workspace: {path}. Paths are relative to the workspace root {}", display_path(&self.root));
        if let Some(hint) = hint {
            message.push_str(&format!(" — did you mean {hint}?"));
        }
        bad(message)
    }

    /// **Check a path the agent named, and say where it really is.** See the module docs for the
    /// rules. An empty path is the root.
    pub fn resolve(&self, path: &str) -> Result<Resolved> {
        let raw = path.trim();
        if raw.contains('\0') {
            return Err(bad("path contains a NUL byte"));
        }
        let raw = if raw.is_empty() { "." } else { raw };
        let candidate = Path::new(raw);
        let lexical = if candidate.is_absolute() {
            normalize_absolute(candidate)
        } else {
            match normalize_relative(candidate) {
                Some(rel) => self.root.join(rel),
                None => return Err(self.escapes(raw, None)),
            }
        };
        let (real, exists) = real_path(&lexical).map_err(|e| bad(format!("{raw}: {e}")))?;
        let Ok(real_rel) = real.strip_prefix(&self.root) else {
            // `/src/main.rs` is the commonest way a model spells `src/main.rs`; say so when that
            // file is there, rather than leaving it to guess what "escapes" means.
            let hint = candidate
                .is_absolute()
                .then(|| raw.trim_start_matches(['/', '\\']))
                .filter(|stripped| !stripped.is_empty() && self.root.join(stripped).exists())
                .map(str::to_string);
            return Err(self.escapes(raw, hint));
        };
        // The name the agent used, normalized — not the symlink's target, which would be a
        // different path than the one it just asked about.
        let rel = match lexical.strip_prefix(&self.root) {
            Ok(rel) => slash_path(rel),
            Err(_) => slash_path(real_rel),
        };
        Ok(Resolved { rel, real, exists })
    }

    /// [`Self::resolve`], for a path about to be written, edited or removed: never the root
    /// itself, and never anything inside `.git`.
    pub fn resolve_for_change(&self, path: &str) -> Result<Resolved> {
        let resolved = self.resolve(path)?;
        if resolved.rel == "." {
            return Err(bad("path names the workspace root itself; give a file path inside it"));
        }
        let real_rel = resolved.real.strip_prefix(&self.root).map(slash_path).unwrap_or_default();
        if touches_git(&resolved.rel) || touches_git(&real_rel) {
            return Err(bad(format!(
                "{} is inside .git, which this API never changes — run git commands with exec instead",
                resolved.rel
            )));
        }
        Ok(resolved)
    }
}

/// Whether any component of a `/`-separated path is `.git`. Case-blind, because the default macOS
/// and Windows filesystems are, and `.GIT/config` is the same file there.
fn touches_git(rel: &str) -> bool {
    rel.split('/').any(|part| part.eq_ignore_ascii_case(".git"))
}

/// The repository a directory is in: where its `.git` is, and the git directory it names.
struct GitRepo {
    top: PathBuf,
    git_dir: PathBuf,
}

/// The nearest `.git` at or above `dir`. A `.git` FILE is a worktree or a submodule pointing at
/// its real git directory (`gitdir: …`, relative to the file's own folder); that is followed.
fn find_git(dir: &Path) -> Option<GitRepo> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else { continue };
        if meta.is_dir() {
            return Some(GitRepo { top: ancestor.to_path_buf(), git_dir: dot_git });
        }
        if meta.is_file() {
            let pointer = std::fs::read_to_string(&dot_git).ok()?;
            let target = pointer.lines().find_map(|l| l.trim().strip_prefix("gitdir:")).map(str::trim)?;
            let git_dir = ancestor.join(target);
            return Some(GitRepo { top: ancestor.to_path_buf(), git_dir });
        }
    }
    None
}

/// The checked-out branch, read from `HEAD` without running git: `ref: refs/heads/main` is
/// `main`, and a detached HEAD is the commit's short hash.
fn read_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        return Some(reference.strip_prefix("refs/heads/").unwrap_or(reference).to_string()).filter(|b| !b.is_empty());
    }
    let is_hash = head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit());
    is_hash.then(|| head[..7].to_string())
}

// ---------------------------------------------------------------------------------------------
// Opening a folder, and the folder browser.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct OpenResponse {
    pub root: String,
    pub name: String,
    /// The folder is a git repository, or inside one.
    pub is_git: bool,
    pub branch: Option<String>,
}

/// What the workspace header shows: the canonical root, its name, and the branch.
pub fn open(root: &str) -> Result<OpenResponse> {
    let workspace = Workspace::open(root)?;
    let root = workspace.root();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| display_path(root));
    let repo = find_git(root);
    let branch = repo.as_ref().and_then(|r| read_branch(&r.git_dir));
    Ok(OpenResponse { root: display_path(root), name, is_git: repo.is_some(), branch })
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_git: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowseResponse {
    pub path: String,
    pub parent: Option<String>,
    pub home: String,
    pub entries: Vec<BrowseEntry>,
}

/// **The subdirectories of a directory, for the folder picker.** `path` defaults to the home
/// directory; dot-directories are left out unless `hidden`.
pub fn browse(path: Option<&str>, hidden: bool) -> Result<BrowseResponse> {
    let home = home_dir().ok_or_else(|| bad("the home directory is not known (HOME is not set)"))?;
    let requested = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => expand_home(p),
        None => home.clone(),
    };
    if !requested.is_absolute() {
        return Err(bad(format!("path must be an absolute directory, got {:?}", requested.display().to_string())));
    }
    let dir = std::fs::canonicalize(&requested).map_err(|e| bad(format!("{}: {e}", requested.display())))?;
    if !dir.is_dir() {
        return Err(bad(format!("{} is not a directory", display_path(&dir))));
    }
    let listing = std::fs::read_dir(&dir).map_err(|e| bad(listing_error(&dir, &e)))?;
    let mut entries = Vec::new();
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let is_dir = match entry.file_type() {
            Ok(t) if t.is_dir() => true,
            Ok(t) if t.is_symlink() => std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
            _ => false,
        };
        if !is_dir {
            continue;
        }
        let is_git = !skip_git_probe(&dir, &home, &name) && std::fs::symlink_metadata(path.join(".git")).is_ok();
        entries.push(BrowseEntry { name, path: display_path(&path), is_dir, is_git });
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    Ok(BrowseResponse { path: display_path(&dir), parent: dir.parent().map(display_path), home: display_path(&home), entries })
}

fn listing_error(dir: &Path, e: &std::io::Error) -> String {
    let mut message = format!("could not list {}: {e}", display_path(dir));
    if cfg!(target_os = "macos") && e.kind() == std::io::ErrorKind::PermissionDenied {
        message.push_str(
            ". macOS has not given MISAKA Studio access to this folder — allow it under System Settings → \
             Privacy & Security → Files and Folders",
        );
    }
    message
}

/// **Folders whose `.git` is not looked for.** On macOS, `stat`-ing `~/Documents/.git` is a read
/// INSIDE a privacy-protected folder, and each one asks the person for permission — so opening
/// the picker at home would greet them with three dialogs about folders they did not click. The
/// cost of skipping is an `is_git: false` on a folder that is rarely a repository.
fn skip_git_probe(dir: &Path, home: &Path, name: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    const PROTECTED_IN_HOME: &[&str] = &["Desktop", "Documents", "Downloads", "Library", "Movies", "Music", "Pictures"];
    (dir == home && PROTECTED_IN_HOME.contains(&name)) || dir == Path::new("/Volumes") || dir.starts_with(home.join("Library"))
}

// ---------------------------------------------------------------------------------------------
// Walking: ignore rules, the tree, the fuzzy file finder.
// ---------------------------------------------------------------------------------------------

/// Which ignore rules apply under a workspace root.
struct Rules {
    root: PathBuf,
    /// Inside a git repository: `.gitignore` files apply from the root up to the repository's top
    /// and no further. Outside one: only the root's own and those below it — never a
    /// `~/.gitignore` that belongs to someone's dotfiles.
    in_repo: bool,
    /// No ignore file governs this root, so the usual heavy directories are skipped by name.
    skip_heavy: bool,
}

impl Rules {
    fn for_root(root: &Path) -> Rules {
        let has_ignore_file = |dir: &Path| dir.join(".gitignore").is_file() || dir.join(".ignore").is_file();
        let repo = find_git(root);
        let governed = match &repo {
            Some(repo) => root.ancestors().take_while(|d| d.starts_with(&repo.top)).any(has_ignore_file),
            None => has_ignore_file(root),
        };
        Rules { root: root.to_path_buf(), in_repo: repo.is_some(), skip_heavy: !governed }
    }

    fn walker(&self, start: &Path, max_depth: Option<usize>) -> ignore::WalkBuilder {
        let mut builder = ignore::WalkBuilder::new(start);
        builder
            .hidden(false)
            .ignore(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(self.in_repo)
            .parents(self.in_repo)
            .follow_links(false)
            .current_dir(&self.root)
            .max_depth(max_depth);
        let skip_heavy = self.skip_heavy;
        builder.filter_entry(move |entry| {
            // The start of a walk is always walked: a workspace may itself be called `build`.
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            if VCS_DIRS.iter().any(|vcs| name.eq_ignore_ascii_case(vcs)) || name == ".DS_Store" {
                return false;
            }
            !(skip_heavy && entry.file_type().is_some_and(|t| t.is_dir()) && HEAVY_DIRS.contains(&name.as_ref()))
        });
        builder
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TreeEntry {
    /// Relative to the workspace root, `/`-separated.
    pub path: String,
    pub is_dir: bool,
    /// Bytes, for a file; `null` for a directory.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TreeResponse {
    pub root: String,
    pub path: String,
    pub entries: Vec<TreeEntry>,
    pub truncated: bool,
}

/// Sort key for explorer order: per component, directories before files, then by name
/// case-insensitively. A path sorts right after its parent, so the list reads as a tree.
type ExplorerKey = Vec<(bool, String, String)>;

fn explorer_key(path: &str, is_dir: bool) -> ExplorerKey {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    parts.iter().enumerate().map(|(i, part)| (i == last && !is_dir, part.to_lowercase(), part.to_string())).collect()
}

/// **The folder, as a tree**: up to `depth` levels below `path`, ignore rules applied.
///
/// Explorer order — within each directory, its subdirectories first, then its files, by name — so
/// the flat list is already a depth-first rendering of the tree. When there are more than `limit`
/// entries, the shallower ones are kept first: a listing that stops short should show all of the
/// top level and part of the next, not all of the first subfolder and nothing after it.
pub fn tree(workspace: &Workspace, path: Option<&str>, depth: Option<usize>, limit: Option<usize>) -> Result<TreeResponse> {
    let depth = depth.unwrap_or(TREE_DEFAULT_DEPTH).clamp(1, TREE_MAX_DEPTH);
    let limit = limit.unwrap_or(TREE_DEFAULT_LIMIT).clamp(1, TREE_MAX_LIMIT);
    let target = workspace.resolve(path.unwrap_or("."))?;
    if !target.exists {
        return Err(bad(format!("{} does not exist", target.rel)));
    }
    if !target.real.is_dir() {
        return Err(bad(format!("{} is a file, not a directory — read it instead", target.rel)));
    }
    let rules = Rules::for_root(workspace.root());
    let mut found: Vec<(usize, ExplorerKey, TreeEntry)> = Vec::new();
    let mut truncated = false;
    for item in rules.walker(&target.real, Some(depth)).build() {
        let Ok(entry) = item else { continue };
        if entry.depth() == 0 {
            continue;
        }
        if found.len() >= WALK_CAP {
            truncated = true;
            break;
        }
        let Ok(rel) = entry.path().strip_prefix(workspace.root()) else { continue };
        let (is_dir, size) = match entry.file_type() {
            Some(t) if t.is_dir() => (true, None),
            Some(t) if t.is_file() => (false, entry.metadata().ok().map(|m| m.len())),
            // A symlink is shown as what it points at; a dangling one as a file of no known size.
            _ => match std::fs::metadata(entry.path()) {
                Ok(m) if m.is_dir() => (true, None),
                Ok(m) => (false, Some(m.len())),
                Err(_) => (false, None),
            },
        };
        let path = slash_path(rel);
        found.push((entry.depth(), explorer_key(&path, is_dir), TreeEntry { path, is_dir, size }));
    }
    if found.len() > limit {
        truncated = true;
        found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        found.truncate(limit);
    }
    found.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(TreeResponse {
        root: display_path(workspace.root()),
        path: target.rel,
        entries: found.into_iter().map(|(_, _, entry)| entry).collect(),
        truncated,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct FilesResponse {
    pub paths: Vec<String>,
}

/// **The `@`-mention finder**: files whose path contains the query's characters in order, best
/// first. An empty query lists the most recently modified files — what someone is most likely to
/// mention next.
pub fn files(workspace: &Workspace, query: &str, limit: Option<usize>) -> Result<FilesResponse> {
    let limit = limit.unwrap_or(FILES_DEFAULT_LIMIT).clamp(1, FILES_MAX_LIMIT);
    let query: Vec<char> =
        query.chars().filter(|c| !c.is_whitespace()).map(|c| if c == '\\' { '/' } else { fold_case(c) }).take(128).collect();
    let rules = Rules::for_root(workspace.root());
    let mut candidates: Vec<(String, Option<SystemTime>)> = Vec::new();
    for item in rules.walker(workspace.root(), None).build() {
        let Ok(entry) = item else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if candidates.len() >= WALK_CAP {
            break;
        }
        let Ok(rel) = entry.path().strip_prefix(workspace.root()) else { continue };
        let modified = if query.is_empty() { entry.metadata().ok().and_then(|m| m.modified().ok()) } else { None };
        candidates.push((slash_path(rel), modified));
    }

    let paths = if query.is_empty() {
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidates.into_iter().take(limit).map(|(path, _)| path).collect()
    } else {
        let mut scored: Vec<(i64, String)> =
            candidates.into_iter().filter_map(|(path, _)| score_path(&query, &path).map(|score| (score, path))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, path)| path).collect()
    };
    Ok(FilesResponse { paths })
}

fn fold_case(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

const SCORE_MATCH: i64 = 16;
const BONUS_BOUNDARY: i64 = 10;
const BONUS_DELIMITER: i64 = 8;
const BONUS_CAMEL: i64 = 7;
const BONUS_CONSECUTIVE: i64 = 12;
const PENALTY_GAP_START: i64 = 3;
const PENALTY_GAP_EXTEND: i64 = 1;
const BONUS_IN_NAME: i64 = 40;
const BONUS_EXACT_NAME: i64 = 120;
const NO_MATCH: i64 = i64::MIN / 4;

/// What a match at `text[j]` is worth beyond the match itself: the start of a path segment, of a
/// word, or of a camelCase hump is where people's abbreviations begin.
fn position_bonus(text: &[char], j: usize) -> i64 {
    let Some(&previous) = j.checked_sub(1).and_then(|p| text.get(p)) else { return BONUS_BOUNDARY };
    match previous {
        '/' | '\\' => BONUS_BOUNDARY,
        '_' | '-' | '.' | ' ' => BONUS_DELIMITER,
        p if p.is_lowercase() && text[j].is_uppercase() => BONUS_CAMEL,
        p if !p.is_alphanumeric() => BONUS_DELIMITER,
        _ => 0,
    }
}

/// **The best way to find `query` in `text` as a subsequence**, scored, or `None` when it is not
/// one. Smith-Waterman-shaped (fzf's v2): a match earns its position's bonus, consecutive matches
/// earn more, and gaps between them cost — so `wsrs` finds `src/workspace.rs` by its word starts
/// rather than by the first `w`, `s`, `r`, `s` the path happens to contain.
fn subsequence_score(query: &[char], text: &[char]) -> Option<i64> {
    let (m, n) = (query.len(), text.len());
    if m == 0 || m > n {
        return None;
    }
    let lower: Vec<char> = text.iter().map(|c| fold_case(*c)).collect();
    let mut next = 0;
    for c in &lower {
        if next < m && *c == query[next] {
            next += 1;
        }
    }
    if next < m {
        return None;
    }
    let bonus: Vec<i64> = (0..n).map(|j| position_bonus(text, j)).collect();
    // The first character's bonus counts twice: where an abbreviation starts matters most.
    let mut previous: Vec<i64> = (0..n).map(|j| if lower[j] == query[0] { SCORE_MATCH + 2 * bonus[j] } else { NO_MATCH }).collect();
    for &wanted in &query[1..] {
        let mut current = vec![NO_MATCH; n];
        // The best score of the previous character at k <= j-2, less the gap from there to j.
        let mut after_gap = NO_MATCH;
        for j in 1..n {
            if j >= 2 {
                after_gap = (after_gap - PENALTY_GAP_EXTEND).max(previous[j - 2] - PENALTY_GAP_START);
            }
            if lower[j] != wanted {
                continue;
            }
            let consecutive = if previous[j - 1] > NO_MATCH / 2 { previous[j - 1] + BONUS_CONSECUTIVE } else { NO_MATCH };
            let best = consecutive.max(after_gap);
            if best > NO_MATCH / 2 {
                current[j] = best + SCORE_MATCH + bonus[j];
            }
        }
        previous = current;
    }
    previous.into_iter().filter(|s| *s > NO_MATCH / 2).max()
}

/// A path's score for a query: the better of a match anywhere in the path and a match inside the
/// file name alone (which is what someone typing `@main` means), with a large bonus for the name
/// exactly, and a small cost per character so the shorter of two equal matches wins.
fn score_path(query: &[char], path: &str) -> Option<i64> {
    let chars: Vec<char> = path.chars().collect();
    let mut best = subsequence_score(query, &chars)?;
    let name_start = chars.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
    let name = &chars[name_start..];
    if let Some(in_name) = subsequence_score(query, name) {
        best = best.max(in_name + BONUS_IN_NAME);
    }
    let name_lower: String = name.iter().map(|c| fold_case(*c)).collect();
    let query_text: String = query.iter().collect();
    let stem = name_lower.rsplit_once('.').map_or(name_lower.as_str(), |(stem, _)| stem);
    if name_lower == query_text {
        best += BONUS_EXACT_NAME;
    } else if stem == query_text {
        best += BONUS_EXACT_NAME * 2 / 3;
    }
    Some(best - chars.len() as i64 / 4)
}

// ---------------------------------------------------------------------------------------------
// Reading.
// ---------------------------------------------------------------------------------------------

fn looks_binary(head: &[u8]) -> bool {
    head.iter().take(BINARY_SNIFF_BYTES).any(|b| *b == 0)
}

/// The first bytes of a file, for the binary test, without reading the rest.
fn sniff(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(BINARY_SNIFF_BYTES);
    std::fs::File::open(path)?.take(BINARY_SNIFF_BYTES as u64).read_to_end(&mut head)?;
    Ok(head)
}

/// The longest prefix of `bytes`, at most `max`, that does not end inside a UTF-8 character.
fn utf8_floor(bytes: &[u8], max: usize) -> usize {
    let mut end = max.min(bytes.len());
    while end > 0 && end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    end
}

#[derive(Clone, Debug, Serialize)]
pub struct ReadResponse {
    pub path: String,
    /// The requested lines, exactly as on disk (line endings included), without line numbers.
    pub content: String,
    /// The first and last line in `content`, 1-based and inclusive. `0, 0` for an empty or binary
    /// file.
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub size: u64,
    /// The range was cut short by `max_bytes`, at a line boundary — read on from `end_line + 1`.
    pub truncated: bool,
    pub binary: bool,
}

/// **Lines `start_line..=end_line` of a file**, within `max_bytes`, cut at a line boundary.
///
/// The file is streamed rather than read whole — `total_lines` needs every newline counted, and
/// that must not mean holding a 2 GB log in memory. A single line longer than the budget (minified
/// JavaScript) is the one exception to the line boundary: it comes back cut at a character
/// boundary, because an empty `content` would say nothing at all.
pub fn read(
    workspace: &Workspace,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: Option<usize>,
) -> Result<ReadResponse> {
    let target = workspace.resolve(path)?;
    if !target.exists {
        return Err(bad(format!("{} does not exist", target.rel)));
    }
    let meta = std::fs::metadata(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
    if meta.is_dir() {
        return Err(bad(format!("{} is a directory — list it with tree", target.rel)));
    }
    let max_bytes = max_bytes.unwrap_or(READ_DEFAULT_MAX_BYTES).clamp(1, READ_MAX_BYTES);
    let start = start_line.unwrap_or(1).max(1);
    if let Some(end) = end_line
        && end < start
    {
        return Err(bad(format!("end_line {end} is before start_line {start}")));
    }
    let size = meta.len();
    let file = std::fs::File::open(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);

    let head = reader.fill_buf().map_err(|e| bad(format!("{}: {e}", target.rel)))?;
    let binary = if head.len() >= BINARY_SNIFF_BYTES.min(size as usize) {
        looks_binary(head)
    } else {
        looks_binary(&sniff(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?)
    };
    if binary {
        return Ok(ReadResponse {
            path: target.rel,
            content: String::new(),
            start_line: 0,
            end_line: 0,
            total_lines: 0,
            size,
            truncated: false,
            binary: true,
        });
    }

    let wanted = |line: usize| line >= start && end_line.is_none_or(|end| line <= end);
    let mut content: Vec<u8> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut line = 1usize;
    let mut last_included = 0usize;
    let mut full = false;
    let mut truncated = false;
    let mut any_bytes = false;
    let mut ends_with_newline = false;
    loop {
        let chunk = reader.fill_buf().map_err(|e| bad(format!("{}: {e}", target.rel)))?;
        if chunk.is_empty() {
            break;
        }
        any_bytes = true;
        let length = chunk.len();
        let mut at = 0;
        while at < length {
            let (segment, line_ends) = match chunk[at..].iter().position(|b| *b == b'\n') {
                Some(offset) => (&chunk[at..at + offset + 1], true),
                None => (&chunk[at..], false),
            };
            at += segment.len();
            if !full && wanted(line) {
                pending.extend_from_slice(segment);
                if content.len() + pending.len() > max_bytes {
                    if content.is_empty() {
                        let cut = utf8_floor(&pending, max_bytes);
                        content.extend_from_slice(&pending[..cut]);
                        last_included = line;
                    }
                    pending.clear();
                    full = true;
                    truncated = true;
                } else if line_ends {
                    content.append(&mut pending);
                    last_included = line;
                }
            }
            if line_ends {
                line += 1;
            }
            ends_with_newline = line_ends;
        }
        reader.consume(length);
    }
    // A last line with no newline after it is still a line.
    if !pending.is_empty() {
        content.append(&mut pending);
        last_included = line;
    }
    let total_lines = if !any_bytes {
        0
    } else if ends_with_newline {
        line - 1
    } else {
        line
    };
    if total_lines > 0 && start > total_lines {
        return Err(bad(format!("start_line {start} is past the end of {}, which has {total_lines} lines", target.rel)));
    }
    let (start_line, end_line) = if last_included == 0 { (0, 0) } else { (start, last_included) };
    Ok(ReadResponse {
        path: target.rel,
        content: String::from_utf8_lossy(&content).into_owned(),
        start_line,
        end_line,
        total_lines,
        size,
        truncated,
        binary: false,
    })
}

// ---------------------------------------------------------------------------------------------
// Searching.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SearchRequest {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    /// `false` searches for the pattern literally.
    pub regex: bool,
    pub case_insensitive: bool,
    pub max_results: Option<usize>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: String,
    /// 1-based.
    pub line: usize,
    /// The matching line, trimmed, at most 300 characters around the match.
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResponse {
    pub matches: Vec<SearchMatch>,
    /// More matches existed, or the scan stopped at its file cap.
    pub truncated: bool,
    pub files_scanned: usize,
}

/// A `glob` filter: one or more patterns separated by commas or spaces (outside `{…}`), `!` to
/// exclude. A pattern with no `/` also matches the file name alone, as in a `.gitignore`, so
/// `*.rs` finds `src/main.rs`; each pattern is tried against the path from the workspace root and
/// from the search's own `path`.
struct GlobFilter {
    rules: Vec<(globset::GlobMatcher, bool, bool)>,
    has_includes: bool,
}

impl GlobFilter {
    fn parse(spec: &str) -> Result<Option<GlobFilter>> {
        let mut rules = Vec::new();
        for raw in split_globs(spec) {
            let (exclude, pattern) = match raw.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, raw.as_str()),
            };
            let pattern = pattern.trim_start_matches("./");
            let anchored = pattern.starts_with('/');
            let mut pattern = pattern.trim_start_matches('/').to_string();
            if pattern.is_empty() {
                continue;
            }
            if pattern.ends_with('/') {
                pattern.push_str("**");
            }
            let by_name = !anchored && !pattern.contains('/');
            let matcher = globset::GlobBuilder::new(&pattern)
                .literal_separator(false)
                .build()
                .map_err(|e| bad(format!("invalid glob {raw:?}: {e}")))?
                .compile_matcher();
            rules.push((matcher, exclude, by_name));
        }
        if rules.is_empty() {
            return Ok(None);
        }
        let has_includes = rules.iter().any(|(_, exclude, _)| !exclude);
        Ok(Some(GlobFilter { rules, has_includes }))
    }

    fn allows(&self, from_root: &str, from_base: &str) -> bool {
        let name = from_root.rsplit('/').next().unwrap_or(from_root);
        let hit =
            |m: &globset::GlobMatcher, by_name: bool| m.is_match(from_root) || m.is_match(from_base) || (by_name && m.is_match(name));
        if self.rules.iter().any(|(m, exclude, by_name)| *exclude && hit(m, *by_name)) {
            return false;
        }
        !self.has_includes || self.rules.iter().any(|(m, exclude, by_name)| !*exclude && hit(m, *by_name))
    }
}

/// Split on commas and whitespace that are not inside braces: `*.{ts,tsx}, !*.d.ts` is two
/// patterns, not three.
fn split_globs(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for c in spec.chars() {
        match c {
            '{' => {
                depth += 1;
                current.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut current)),
            c if c.is_whitespace() && depth == 0 => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// The line a hit is reported with: trimmed, and when it is long, a window of
/// [`SEARCH_LINE_CHARS`] characters that starts a little before the match.
fn excerpt(line: &str, regex: &regex::bytes::Regex) -> String {
    let trimmed = line.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= SEARCH_LINE_CHARS {
        return trimmed.to_string();
    }
    let match_byte = regex.find(trimmed.as_bytes()).map_or(0, |m| m.start());
    let match_char = trimmed.get(..match_byte).map_or(0, |prefix| prefix.chars().count());
    let room = SEARCH_LINE_CHARS - 2; // for the two ellipses
    let start = match_char.saturating_sub(room / 3).min(chars.len() - room);
    let end = (start + room).min(chars.len());
    let mut text = String::new();
    if start > 0 {
        text.push('…');
    }
    text.extend(&chars[start..end]);
    if end < chars.len() {
        text.push('…');
    }
    text
}

/// **ripgrep, in-process**: every line matching `pattern` under `path`, ignore rules and the
/// `glob` filter applied, binary files and files over 1 MiB skipped.
///
/// Line by line, like `rg` without `-U`: `^` and `$` are the line's, and a match never spans two
/// lines. Results are in path order, so the same search answers the same way twice.
pub fn search(workspace: &Workspace, request: &SearchRequest) -> Result<SearchResponse> {
    if request.pattern.is_empty() {
        return Err(bad("pattern is empty"));
    }
    let max_results = request.max_results.unwrap_or(SEARCH_DEFAULT_MAX_RESULTS).clamp(1, SEARCH_MAX_RESULTS);
    let source = if request.regex { request.pattern.clone() } else { regex::escape(&request.pattern) };
    let line_regex = regex::bytes::RegexBuilder::new(&source)
        .case_insensitive(request.case_insensitive)
        .build()
        .map_err(|e| bad(format!("invalid regex (set regex: false to search for the text literally): {e}")))?;
    // A whole-file pass first, so the files without a match — nearly all of them — cost one scan
    // rather than one per line. Line anchors become multi-line ones, which can only find MORE than
    // the per-line pass; `\A` and `\z` are the exceptions, and a pattern using them skips it.
    let file_regex = if source.contains("\\A") || source.contains("\\z") {
        None
    } else {
        regex::bytes::RegexBuilder::new(&source).case_insensitive(request.case_insensitive).multi_line(true).crlf(true).build().ok()
    };
    let filter = request.glob.as_deref().map(GlobFilter::parse).transpose()?.flatten();
    let base = workspace.resolve(request.path.as_deref().unwrap_or("."))?;
    if !base.exists {
        return Err(bad(format!("{} does not exist", base.rel)));
    }
    let base_prefix = if base.rel == "." { String::new() } else { format!("{}/", base.rel) };

    let mut scan = Scan {
        line_regex: &line_regex,
        file_regex: file_regex.as_ref(),
        max_results,
        matches: Vec::new(),
        truncated: false,
        files_scanned: 0,
    };
    if base.real.is_file() {
        // A file named outright is searched whatever the glob says, as `rg PATTERN FILE` does.
        scan.file(&base.real, &base.rel);
    } else {
        let rules = Rules::for_root(workspace.root());
        let mut walker = rules.walker(&base.real, None);
        walker.sort_by_file_name(|a, b| a.cmp(b));
        for item in walker.build() {
            let Ok(entry) = item else { continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(workspace.root()) else { continue };
            let rel = slash_path(rel);
            if let Some(filter) = &filter {
                let from_base = rel.strip_prefix(&base_prefix).unwrap_or(&rel);
                if !filter.allows(&rel, from_base) {
                    continue;
                }
            }
            if scan.files_scanned >= SEARCH_SCAN_CAP {
                scan.truncated = true;
                break;
            }
            if !scan.file(entry.path(), &rel) {
                break;
            }
        }
    }
    Ok(SearchResponse { matches: scan.matches, truncated: scan.truncated, files_scanned: scan.files_scanned })
}

/// One search's progress.
struct Scan<'a> {
    line_regex: &'a regex::bytes::Regex,
    file_regex: Option<&'a regex::bytes::Regex>,
    max_results: usize,
    matches: Vec<SearchMatch>,
    truncated: bool,
    files_scanned: usize,
}

impl Scan<'_> {
    /// Search one file. `false` once the result cap is reached and the search should stop.
    fn file(&mut self, path: &Path, rel: &str) -> bool {
        let Ok(meta) = std::fs::metadata(path) else { return true };
        if meta.len() > SEARCH_MAX_FILE_BYTES {
            return true;
        }
        let Ok(bytes) = std::fs::read(path) else { return true };
        if looks_binary(&bytes) {
            return true;
        }
        self.files_scanned += 1;
        if self.file_regex.is_some_and(|r| !r.is_match(&bytes)) {
            return true;
        }
        for (index, raw) in bytes.split(|b| *b == b'\n').enumerate() {
            let line = raw.strip_suffix(b"\r").unwrap_or(raw);
            if !self.line_regex.is_match(line) {
                continue;
            }
            if self.matches.len() == self.max_results {
                self.truncated = true;
                return false;
            }
            let text = excerpt(&String::from_utf8_lossy(line), self.line_regex);
            self.matches.push(SearchMatch { path: rel.to_string(), line: index + 1, text });
        }
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Changing files.
// ---------------------------------------------------------------------------------------------

/// Write `bytes` to `target` all at once or not at all: a temporary file beside it, synced, given
/// the old file's permissions, then renamed over it. A crash, a full disk or a killed process
/// leaves the old file or the new one, never half of the new one.
fn write_atomic(target: &Path, bytes: &[u8], permissions: Option<std::fs::Permissions>) -> std::io::Result<()> {
    let dir = target.parent().ok_or_else(|| std::io::Error::other("the file has no parent directory"))?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.misaka-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if let Some(permissions) = permissions {
            std::fs::set_permissions(&temp, permissions)?;
        }
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The checks every change makes on a file that exists: not a directory, not read-only.
fn changeable(target: &Resolved) -> Result<std::fs::Metadata> {
    let meta = std::fs::metadata(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
    if meta.is_dir() {
        return Err(bad(format!("{} is a directory", target.rel)));
    }
    if meta.permissions().readonly() {
        return Err(bad(format!("{} is read-only; it was not changed", target.rel)));
    }
    Ok(meta)
}

#[derive(Clone, Debug, Serialize)]
pub struct WriteResponse {
    pub path: String,
    pub created: bool,
    /// What the file held before: `null` for a new file, or one over 2 MiB.
    pub previous: Option<String>,
    pub content: String,
    pub bytes: usize,
}

/// **Create or replace a file** with `content`, making its folders as needed. A binary file is
/// never overwritten — that is an image or a database the model has mistaken for text.
pub fn write(workspace: &Workspace, path: &str, content: String) -> Result<WriteResponse> {
    if content.len() > WRITE_MAX_BYTES {
        return Err(bad(format!("content is {} bytes; write takes at most {WRITE_MAX_BYTES}", content.len())));
    }
    let target = workspace.resolve_for_change(path)?;
    let mut destination = target.real.clone();
    let (created, previous, permissions) = if target.exists {
        let meta = changeable(&target)?;
        let previous = if meta.len() > PREVIOUS_MAX_BYTES {
            if looks_binary(&sniff(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?) {
                return Err(refuse_binary(&target.rel));
            }
            None
        } else {
            let old = std::fs::read(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
            if looks_binary(&old) {
                return Err(refuse_binary(&target.rel));
            }
            Some(String::from_utf8_lossy(&old).into_owned())
        };
        (false, previous, Some(meta.permissions()))
    } else {
        let parent = target.real.parent().ok_or_else(|| bad(format!("{} has no parent directory", target.rel)))?;
        std::fs::create_dir_all(parent).map_err(|e| bad(format!("could not create the folders for {}: {e}", target.rel)))?;
        // Made through whatever the path's existing part leads to; checked again now that it is
        // all there.
        let parent = std::fs::canonicalize(parent).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
        if !parent.starts_with(workspace.root()) {
            return Err(workspace.escapes(&target.rel, None));
        }
        if let Some(name) = target.real.file_name() {
            destination = parent.join(name);
        }
        (true, None, None)
    };
    write_atomic(&destination, content.as_bytes(), permissions).map_err(|e| bad(format!("could not write {}: {e}", target.rel)))?;
    Ok(WriteResponse { path: target.rel, created, previous, bytes: content.len(), content })
}

fn refuse_binary(rel: &str) -> Error {
    bad(format!("{rel} is a binary file; it was not overwritten"))
}

#[derive(Clone, Debug, Serialize)]
pub struct EditResponse {
    pub path: String,
    pub previous: String,
    pub content: String,
    pub replacements: usize,
}

/// **Replace `old_string` with `new_string` in a file**: exactly once unless `replace_all`.
/// See [`apply_edit`] for how forgiving the match is, and why.
pub fn edit(workspace: &Workspace, path: &str, old_string: &str, new_string: &str, replace_all: bool) -> Result<EditResponse> {
    let target = workspace.resolve_for_change(path)?;
    if !target.exists {
        return Err(bad(format!("{} does not exist — use write to create it", target.rel)));
    }
    let meta = changeable(&target)?;
    if meta.len() > WRITE_MAX_BYTES as u64 {
        return Err(bad(format!("{} is {} bytes; edit changes files up to {WRITE_MAX_BYTES} bytes", target.rel, meta.len())));
    }
    let bytes = std::fs::read(&target.real).map_err(|e| bad(format!("{}: {e}", target.rel)))?;
    if looks_binary(&bytes) {
        return Err(bad(format!("{} is a binary file; edit only changes text", target.rel)));
    }
    let previous = String::from_utf8(bytes)
        .map_err(|_| bad(format!("{} is not valid UTF-8, and rewriting it as text would corrupt it", target.rel)))?;
    let outcome = apply_edit(&previous, old_string, new_string, replace_all, &target.rel)?;
    if outcome.content.len() > WRITE_MAX_BYTES {
        return Err(bad(format!(
            "the edited {} would be {} bytes; the limit is {WRITE_MAX_BYTES}",
            target.rel,
            outcome.content.len()
        )));
    }
    write_atomic(&target.real, outcome.content.as_bytes(), Some(meta.permissions()))
        .map_err(|e| bad(format!("could not write {}: {e}", target.rel)))?;
    Ok(EditResponse { path: target.rel, previous, content: outcome.content, replacements: outcome.replacements })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOutcome {
    pub content: String,
    pub replacements: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineEndings {
    None,
    Lf,
    Crlf,
    Mixed,
}

fn line_endings(text: &str) -> LineEndings {
    let newlines = text.matches('\n').count();
    let crlf = text.matches("\r\n").count();
    match (newlines, crlf) {
        (0, _) => LineEndings::None,
        (n, c) if n == c => LineEndings::Crlf,
        (_, 0) => LineEndings::Lf,
        _ => LineEndings::Mixed,
    }
}

fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// Occurrences of `needle`, overlapping ones included — `aa` is in `aaa` twice, and "replace the
/// one place it occurs" is not a well-defined instruction there.
fn count_overlapping(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        count += 1;
        let at = from + offset;
        from = at + haystack[at..].chars().next().map_or(1, char::len_utf8);
        if from >= haystack.len() {
            break;
        }
    }
    count
}

fn not_unique(count: usize, rel: &str) -> Error {
    bad(format!("old_string matches {count} places in {rel}; include more surrounding lines to make it unique, or set replace_all"))
}

/// **Exact replacement, and the two near misses that are safe to forgive.**
///
/// In order, stopping at the first that finds anything:
///
/// 1. `old_string` exactly as sent.
/// 2. The same text in the file's line endings — a model writes `\n`; a Windows checkout has `\r\n`.
/// 3. Line-end whitespace ignored: every line of `old_string` but the last, and every line of
///    the file, compared without trailing spaces, tabs and `\r`. A small model routinely drops the
///    trailing space a file has or adds one it does not. Its last line keeps its trailing
///    whitespace, because a needle that ends mid-line may mean it — unless the match ends where
///    the file's line ends, when it cannot. The whitespace the match covers in the file goes with
///    it, so the result is what `new_string` says.
///
/// Indentation is never forgiven: in Python or YAML a different indent is a different program. A
/// match that differs only there is reported with its line number instead, which is what lets the
/// model fix its own request.
///
/// `new_string` is written in the file's line endings whenever the file has only one kind.
/// Uniqueness is decided in whichever comparison found the match.
pub fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool, rel: &str) -> Result<EditOutcome> {
    if old.is_empty() {
        return Err(bad(format!(
            "old_string is empty. To create {rel} use write; to insert text, put the line it goes after in old_string and \
             that line plus the new text in new_string"
        )));
    }
    if old == new {
        return Err(bad(format!("old_string and new_string are identical — there is nothing to change in {rel}")));
    }
    let style = line_endings(content);
    let replacement: Cow<str> = match style {
        LineEndings::Crlf if new.contains('\n') => Cow::Owned(to_crlf(new)),
        LineEndings::Lf if new.contains("\r\n") => Cow::Owned(to_lf(new)),
        _ => Cow::Borrowed(new),
    };

    if let Some(outcome) = replace_exact(content, old, &replacement, replace_all, rel)? {
        return Ok(outcome);
    }
    let converted = match style {
        LineEndings::Crlf | LineEndings::Mixed if old.contains('\n') => Some(to_crlf(old)),
        LineEndings::Lf if old.contains("\r\n") => Some(to_lf(old)),
        _ => None,
    };
    if let Some(converted) = converted.filter(|c| c != old)
        && let Some(outcome) = replace_exact(content, &converted, &replacement, replace_all, rel)?
    {
        return Ok(outcome);
    }
    if let Some(outcome) = replace_loose(content, old, &replacement, replace_all, rel)? {
        return Ok(outcome);
    }

    let mut message = format!("old_string was not found in {rel}. Read the file again and copy the exact text, including whitespace.");
    if let Some(line) = indentation_only_match(content, old) {
        message.push_str(&format!(
            " Text that matches it except for indentation starts at line {line} — copy that indentation exactly (tabs and spaces \
             are different characters)."
        ));
    }
    Err(bad(message))
}

fn replace_exact(content: &str, needle: &str, replacement: &str, replace_all: bool, rel: &str) -> Result<Option<EditOutcome>> {
    match count_overlapping(content, needle) {
        0 => Ok(None),
        1 => Ok(Some(EditOutcome { content: content.replacen(needle, replacement, 1), replacements: 1 })),
        _ if replace_all => {
            let replacements = content.matches(needle).count();
            Ok(Some(EditOutcome { content: content.replace(needle, replacement), replacements }))
        }
        count => Err(not_unique(count, rel)),
    }
}

/// A file with line-end whitespace removed and every line ending made `\n`, remembering, for each
/// byte kept, where it was. A file that does not end in a newline is given one, at its end, so a
/// needle that ends in `\n` still matches its last line.
struct LooseText {
    text: String,
    /// `origin[i]` is the byte of the original that `text[i]` came from; the added final newline
    /// maps to the original's length.
    origin: Vec<usize>,
    original_len: usize,
}

impl LooseText {
    fn of(content: &str) -> LooseText {
        let bytes = content.as_bytes();
        let mut text = Vec::with_capacity(bytes.len() + 1);
        let mut origin = Vec::with_capacity(bytes.len() + 1);
        let mut line_start = 0;
        loop {
            let newline = bytes[line_start..].iter().position(|b| *b == b'\n').map(|p| line_start + p);
            let line_end = newline.unwrap_or(bytes.len());
            let mut kept_end = line_end;
            while kept_end > line_start && matches!(bytes[kept_end - 1], b' ' | b'\t' | b'\r') {
                kept_end -= 1;
            }
            for (offset, byte) in bytes[line_start..kept_end].iter().enumerate() {
                text.push(*byte);
                origin.push(line_start + offset);
            }
            match newline {
                Some(at) => {
                    text.push(b'\n');
                    origin.push(at);
                    line_start = at + 1;
                    if line_start == bytes.len() {
                        break;
                    }
                }
                None => {
                    text.push(b'\n');
                    origin.push(bytes.len());
                    break;
                }
            }
        }
        // Only ASCII bytes were removed or added, so this is still UTF-8.
        let text = String::from_utf8(text).unwrap_or_default();
        LooseText { text, origin, original_len: bytes.len() }
    }

    /// Non-overlapping matches, as byte ranges of the ORIGINAL. With `at_line_end`, only matches
    /// followed by the end of a line count.
    fn find_all(&self, needle: &str, at_line_end: bool) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        if needle.is_empty() || self.text.is_empty() {
            return spans;
        }
        let mut from = 0;
        while from < self.text.len() {
            let Some(offset) = self.text[from..].find(needle) else { break };
            let start = from + offset;
            let end = start + needle.len();
            if !at_line_end || self.text.as_bytes().get(end) == Some(&b'\n') {
                let last = self.origin[end - 1];
                let original_end = if last >= self.original_len { self.original_len } else { last + 1 };
                spans.push((self.origin[start].min(self.original_len), original_end));
                from = end;
            } else {
                from = start + self.text[start..].chars().next().map_or(1, char::len_utf8);
            }
        }
        spans
    }
}

/// `old_string` as [`LooseText`] sees a file: `\n` line endings, and no trailing whitespace on any
/// line but (unless `trim_last`) the last.
fn loose_needle(old: &str, trim_last: bool) -> String {
    let unified = to_lf(old);
    let parts: Vec<&str> = unified.split('\n').collect();
    let last = parts.len() - 1;
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| if i < last || trim_last { part.trim_end_matches([' ', '\t', '\r']) } else { part })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_loose(content: &str, old: &str, replacement: &str, replace_all: bool, rel: &str) -> Result<Option<EditOutcome>> {
    let loose = LooseText::of(content);
    let keep_last = loose_needle(old, false);
    let mut spans = if keep_last.trim().is_empty() { Vec::new() } else { loose.find_all(&keep_last, false) };
    if spans.is_empty() {
        let trim_last = loose_needle(old, true);
        if trim_last != keep_last && !trim_last.trim().is_empty() {
            spans = loose.find_all(&trim_last, true);
        }
    }
    match spans.len() {
        0 => return Ok(None),
        1 => {}
        _ if replace_all => {}
        count => return Err(not_unique(count, rel)),
    }
    let mut out = String::with_capacity(content.len() + replacement.len());
    let mut copied = 0;
    for (start, end) in &spans {
        out.push_str(&content[copied..*start]);
        out.push_str(replacement);
        copied = *end;
    }
    out.push_str(&content[copied..]);
    Ok(Some(EditOutcome { content: out, replacements: spans.len() }))
}

/// The 1-based line where `old` would match if indentation (and line-end whitespace) did not
/// count — for the not-found message only, never to apply an edit.
fn indentation_only_match(content: &str, old: &str) -> Option<usize> {
    let needle: Vec<&str> = old.lines().map(str::trim).collect();
    let first = needle.iter().position(|l| !l.is_empty())?;
    let last = needle.iter().rposition(|l| !l.is_empty())?;
    let needle = &needle[first..=last];
    let lines: Vec<&str> = content.lines().map(str::trim).collect();
    if needle.len() == 1 {
        return lines.iter().position(|l| l.contains(needle[0])).map(|i| i + 1);
    }
    let final_index = needle.len() - 1;
    (0..=lines.len().saturating_sub(needle.len()))
        .find(|start| {
            needle.iter().enumerate().all(|(k, wanted)| {
                let Some(line) = lines.get(start + k) else { return false };
                match k {
                    0 => line.ends_with(wanted),
                    k if k == final_index => line.starts_with(wanted),
                    _ => line == wanted,
                }
            })
        })
        .map(|start| start + 1)
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoveResponse {
    pub removed: String,
}

/// **Delete a file** — files only, and never in `.git`. What an agent uses to take back a file it
/// created; a directory is refused because "remove what I made" never needs one, and a recursive
/// delete driven by a model is the one mistake that cannot be shown as a diff and undone.
///
/// A symlink is removed as a link: what it points at is not touched.
pub fn remove(workspace: &Workspace, path: &str) -> Result<RemoveResponse> {
    let target = workspace.resolve_for_change(path)?;
    let name = Path::new(&target.rel).file_name().ok_or_else(|| bad(format!("{} is not a file path", target.rel)))?.to_os_string();
    let parent_rel = Path::new(&target.rel).parent().map(slash_path).unwrap_or_else(|| ".".to_string());
    let parent = workspace.resolve(&parent_rel)?;
    if !parent.exists {
        return Err(bad(format!("{} does not exist", target.rel)));
    }
    let entry = parent.real.join(name);
    let meta = std::fs::symlink_metadata(&entry).map_err(|_| bad(format!("{} does not exist", target.rel)))?;
    if meta.is_dir() {
        return Err(bad(format!("{} is a directory; remove deletes single files only", target.rel)));
    }
    std::fs::remove_file(&entry).map_err(|e| bad(format!("could not remove {}: {e}", target.rel)))?;
    Ok(RemoveResponse { removed: target.rel })
}

// ---------------------------------------------------------------------------------------------
// Running commands.
// ---------------------------------------------------------------------------------------------

/// What every command sees beyond the user's own environment: no pager waiting for a keypress, no
/// colour codes in the output a model reads, no git asking for a password on a terminal nobody is
/// watching — and a variable a project's own scripts can test for.
const AGENT_ENV: &[(&str, &str)] = &[
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("TERM", "dumb"),
    ("NO_COLOR", "1"),
    ("CLICOLOR", "0"),
    ("MISAKA_AGENT", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// After the command exits, how long its output pipes may stay open — held by something it left
/// running in the background — before that is killed so the call can return.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// After a timeout's SIGTERM, how long before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Serialize)]
pub struct ExecResponse {
    pub command: String,
    /// `null` when the command was killed (a timeout, or a signal).
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
    /// stdout or stderr was over 100 KiB and has its middle cut out.
    pub truncated: bool,
}

/// One output stream, kept as its first and last halves of [`EXEC_STREAM_CAP`].
///
/// Head AND tail because both ends matter: a compiler's first error is at the top, a test runner's
/// summary at the bottom, and the ten thousand lines between them are what a model's context
/// cannot afford.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl Capture {
    const HEAD: usize = EXEC_STREAM_CAP / 2;
    const TAIL: usize = EXEC_STREAM_CAP - Self::HEAD;

    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        let room = Self::HEAD.saturating_sub(self.head.len()).min(bytes.len());
        let (to_head, rest) = bytes.split_at(room);
        self.head.extend_from_slice(to_head);
        if !rest.is_empty() {
            let rest = &rest[rest.len().saturating_sub(Self::TAIL)..];
            self.tail.extend(rest.iter().copied());
            let excess = self.tail.len().saturating_sub(Self::TAIL);
            self.tail.drain(..excess);
        }
    }

    /// The text, and whether anything was cut out of its middle.
    fn finish(&self) -> (String, bool) {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if self.head.len() + tail.len() == self.total {
            let mut all = self.head.clone();
            all.extend_from_slice(&tail);
            return (String::from_utf8_lossy(&all).into_owned(), false);
        }
        // Cut on character boundaries, so the marker is not flanked by replacement characters...
        let head_end = match std::str::from_utf8(&self.head) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => self.head.len(),
        };
        let tail_start = tail.iter().take(3).take_while(|b| (**b & 0xC0) == 0x80).count();
        // ...and on line boundaries when a newline is near: half a line beside the marker reads as
        // a whole one, and `10` cut out of `10023` is a wrong number rather than a short one. The
        // head's last newline is the marker's own.
        const SNAP: usize = 4096;
        let (head_end, marker_newline) = match self.head[..head_end].iter().rposition(|b| *b == b'\n') {
            Some(newline) if head_end - newline <= SNAP => (newline, 1),
            _ => (head_end, 0),
        };
        let tail_start = match tail[tail_start..].iter().position(|b| *b == b'\n') {
            Some(newline) if newline < SNAP => tail_start + newline + 1,
            _ => tail_start,
        };
        let omitted = self.total - head_end - marker_newline - (tail.len() - tail_start);
        let text = format!(
            "{}\n… [{omitted} bytes omitted] …\n{}",
            String::from_utf8_lossy(&self.head[..head_end]),
            String::from_utf8_lossy(&tail[tail_start..])
        );
        (text, true)
    }
}

/// Where a command's output goes as it is read: a [`Capture`] for [`exec`], the event stream for
/// [`exec_stream`].
trait Sink: Send + 'static {
    fn push(&mut self, bytes: &[u8]);
    /// The pipe closed. Not called for a reader that was stopped.
    fn end(&mut self) {}
}

impl Sink for Arc<Mutex<Capture>> {
    fn push(&mut self, bytes: &[u8]) {
        self.lock().expect("capture lock").push(bytes);
    }
}

fn read_into<R>(stream: Option<R>, mut sink: impl Sink) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let Some(mut stream) = stream else { return };
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.push(&buffer[..n]),
            }
        }
        sink.end();
    })
}

/// The two tasks reading a command's stdout and stderr. Dropping this stops them: a run that is
/// cancelled must not leave a reader waiting on a pipe that something outside the group holds.
struct Readers([tokio::task::JoinHandle<()>; 2]);

impl Readers {
    fn start(child: &mut tokio::process::Child, stdout: impl Sink, stderr: impl Sink) -> Readers {
        Readers([read_into(child.stdout.take(), stdout), read_into(child.stderr.take(), stderr)])
    }

    /// Whether both pipes have closed, waiting up to `within` for them to.
    async fn finished(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.0.iter().all(|h| h.is_finished()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn stop(&self) {
        for reader in &self.0 {
            reader.abort();
        }
    }
}

impl Drop for Readers {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The command, through the user's login shell — so a Studio started from the Dock, whose
/// environment is launchd's and not the terminal's, still finds `cargo`, `node` and `brew`.
#[cfg(unix)]
fn shell_command(command: &str) -> tokio::process::Command {
    let shell = std::env::var_os("SHELL").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file());
    let shell = shell.unwrap_or_else(|| PathBuf::from("/bin/sh"));
    // csh and tcsh take `-l` only as their sole argument.
    let csh = shell.file_name().is_some_and(|n| n == "csh" || n == "tcsh");
    let mut cmd = tokio::process::Command::new(&shell);
    if !csh {
        cmd.arg("-l");
    }
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> tokio::process::Command {
    let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut cmd = tokio::process::Command::new(shell);
    // `cmd` does not parse its command line the way every other program does, so the command goes
    // in verbatim: `/S /C "…"` strips exactly the outer quotes and runs the rest as typed.
    cmd.raw_arg(format!("/S /C \"{command}\""));
    cmd
}

/// **Its own session, so its own process group**: the command and everything it starts can be
/// signalled as one, and none of it has a controlling terminal — a `sudo` or `ssh` that would
/// otherwise wait forever for a password on a terminal nobody is watching fails at once instead.
#[cfg(unix)]
fn detach(cmd: &mut tokio::process::Command) {
    // SAFETY: the closure runs in the forked child just before `exec` and calls only `setsid`,
    // which is async-signal-safe; it touches no memory shared with the parent.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
}

#[cfg(windows)]
fn detach(cmd: &mut tokio::process::Command) {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// The group a command runs in, killed when this is dropped unless disarmed — which is what stops
/// a cancelled request (the client hung up, the server is shutting down) from leaving a test
/// suite running with nobody to read its output.
struct ProcessGroup {
    pid: Option<u32>,
    armed: bool,
}

impl ProcessGroup {
    fn new(pid: Option<u32>) -> Self {
        // 0 and 1 are never a child's process group; `killpg(0, …)` would signal OUR group.
        ProcessGroup { pid: pid.filter(|p| *p > 1), armed: true }
    }

    #[cfg(unix)]
    fn signal(&self, signal: libc::c_int) {
        if let Some(pid) = self.pid {
            // SAFETY: `killpg` takes two integers and reads no memory. The group is the one
            // `setsid` made for this child — its id is the child's pid — so the signal reaches
            // the command and what it started, and nothing else.
            unsafe {
                libc::killpg(pid as libc::pid_t, signal);
            }
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        #[cfg(windows)]
        if let Some(pid) = self.pid {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    /// A timeout: ask, wait a moment, then insist. Returns the shell's exit status.
    async fn stop(&self, child: &mut tokio::process::Child) -> Option<std::process::ExitStatus> {
        #[cfg(unix)]
        self.signal(libc::SIGTERM);
        let status = match tokio::time::timeout(TERM_GRACE, child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => None,
        };
        // The shell may have gone on TERM while what it started did not.
        self.kill();
        match status {
            Some(status) => Some(status),
            None => {
                let _ = child.start_kill();
                child.wait().await.ok()
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.armed {
            self.kill();
        }
    }
}

fn check_command(command: &str) -> Result<()> {
    if command.trim().is_empty() { Err(bad("command is empty")) } else { Ok(()) }
}

/// A command started in a workspace: its shell, the process group the shell leads, and when it
/// began.
struct Running {
    child: tokio::process::Child,
    group: ProcessGroup,
    started: Instant,
}

/// **Start `command` the one way [`exec`] and [`exec_stream`] both start it**: through the login
/// shell, in the root, with stdin closed, the agent environment, and a session of its own.
fn start_command(workspace: &Workspace, command: &str) -> Result<Running> {
    let mut cmd = shell_command(command);
    cmd.current_dir(workspace.root()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for (key, value) in AGENT_ENV {
        cmd.env(key, value);
    }
    // The runtime's own `PWD` would otherwise be inherited, naming a directory the command is
    // not in.
    cmd.env("PWD", workspace.root());
    // These win over NO_COLOR in the tools that read them.
    cmd.env_remove("CLICOLOR_FORCE").env_remove("FORCE_COLOR");
    detach(&mut cmd);

    let started = Instant::now();
    let child = cmd.spawn().map_err(|e| bad(format!("could not start the shell for {command:?}: {e}")))?;
    let group = ProcessGroup::new(child.id());
    Ok(Running { child, group, started })
}

impl Running {
    /// The ending [`exec`] and [`exec_stream`] share — the command, then its output pipes, as
    /// [`exec`] describes. Returns the shell's exit status and whether the timeout ended it.
    async fn finish(&mut self, readers: &Readers, timeout: Duration) -> (Option<std::process::ExitStatus>, bool) {
        let (status, timed_out) = match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(status) => (status.ok(), false),
            Err(_) => (self.group.stop(&mut self.child).await, true),
        };
        if !readers.finished(PIPE_GRACE).await {
            self.group.kill();
            if !readers.finished(PIPE_GRACE).await {
                // Held open by something outside the group (it made its own session); what arrived
                // so far is the answer.
                readers.stop();
            }
        }
        self.group.disarm();
        (status, timed_out)
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// **Run `command` in the workspace**, through the login shell, and return what it printed.
///
/// stdin is closed; each output stream keeps its first and last 50 KiB. On `timeout_secs` the
/// whole process group gets SIGTERM, then SIGKILL a second later. Something the command leaves
/// running in the background is left alone if it let go of the output (`nohup … > log &`), and
/// killed if it is still holding it two seconds after the command finished — otherwise the call
/// could not return.
pub async fn exec(workspace: &Workspace, command: &str, timeout_secs: Option<u64>) -> Result<ExecResponse> {
    check_command(command)?;
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(EXEC_DEFAULT_TIMEOUT_SECS).clamp(1, EXEC_MAX_TIMEOUT_SECS));
    let mut running = start_command(workspace, command)?;
    let stdout = Arc::new(Mutex::new(Capture::default()));
    let stderr = Arc::new(Mutex::new(Capture::default()));
    let readers = Readers::start(&mut running.child, stdout.clone(), stderr.clone());
    let (status, timed_out) = running.finish(&readers, timeout).await;

    let (stdout, out_cut) = stdout.lock().expect("capture lock").finish();
    let (stderr, err_cut) = stderr.lock().expect("capture lock").finish();
    Ok(ExecResponse {
        command: command.to_string(),
        exit_code: if timed_out { None } else { status.and_then(|s| s.code()) },
        stdout,
        stderr,
        timed_out,
        duration_ms: running.elapsed_ms(),
        truncated: out_cut || err_cut,
    })
}

// ---------------------------------------------------------------------------------------------
// Running commands, streamed: the terminal.
// ---------------------------------------------------------------------------------------------

/// One event of a streamed command — a `data:` line of the `exec/stream` endpoint.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecEvent {
    Stdout {
        data: String,
    },
    Stderr {
        data: String,
    },
    /// The last event of a command that started. `exit_code` is `null` when it was killed.
    Exit {
        exit_code: Option<i32>,
        timed_out: bool,
        duration_ms: u64,
    },
    /// The command could not be started; nothing follows.
    Error {
        message: String,
    },
}

/// **Run `command` in the workspace and stream what it prints, as it prints it** — the terminal.
///
/// Started as [`exec`] starts a command and ended as it ends one, with a longer timeout; what
/// differs is the output, which arrives as `stdout`/`stderr` events (UTF-8, a character split
/// between two reads held until it is whole) up to [`EXEC_STREAM_OUTPUT_CAP`], and then an `exit`.
///
/// **Dropping the stream stops the command**: the whole group gets SIGTERM, then SIGKILL a
/// second later — the same as a timeout. That is how the terminal's Ctrl+C works: the UI aborts
/// the request, the server drops the response, and nothing it started is left running unseen.
pub fn exec_stream(
    workspace: &Workspace,
    command: &str,
    timeout_secs: Option<u64>,
) -> Result<impl futures_util::Stream<Item = ExecEvent> + Send + 'static> {
    check_command(command)?;
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(EXEC_STREAM_DEFAULT_TIMEOUT_SECS).clamp(1, EXEC_STREAM_MAX_TIMEOUT_SECS));
    // Unbounded, because the output cap bounds it: never more than EXEC_STREAM_OUTPUT_CAP waits
    // here for a slow reader, and the command is never held up by one.
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    match start_command(workspace, command) {
        Ok(running) => {
            tokio::spawn(stream_command(running, timeout, events));
        }
        Err(error) => {
            let _ = events.send(ExecEvent::Error { message: error.to_string() });
        }
    }
    Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(receiver))
}

async fn stream_command(mut running: Running, timeout: Duration, events: tokio::sync::mpsc::UnboundedSender<ExecEvent>) {
    let budget = Arc::new(OutputBudget::new(EXEC_STREAM_OUTPUT_CAP));
    let readers = Readers::start(
        &mut running.child,
        Forward::new(Pipe::Stdout, budget.clone(), events.clone()),
        Forward::new(Pipe::Stderr, budget, events.clone()),
    );
    let finished = tokio::select! {
        biased;
        () = events.closed() => None,
        outcome = running.finish(&readers, timeout) => Some(outcome),
    };
    match finished {
        Some((status, timed_out)) => {
            let exit_code = if timed_out { None } else { status.and_then(|s| s.code()) };
            let _ = events.send(ExecEvent::Exit { exit_code, timed_out, duration_ms: running.elapsed_ms() });
        }
        // Nobody is reading any more: the stream was dropped. Stop the command as a timeout
        // would; dropping `readers` then stops the readers.
        None => {
            running.group.stop(&mut running.child).await;
            running.group.disarm();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Pipe {
    Stdout,
    Stderr,
}

/// What is left of [`EXEC_STREAM_OUTPUT_CAP`], shared by a command's stdout and stderr.
struct OutputBudget {
    remaining: AtomicUsize,
    noted: AtomicBool,
}

impl OutputBudget {
    fn new(bytes: usize) -> Self {
        OutputBudget { remaining: AtomicUsize::new(bytes), noted: AtomicBool::new(false) }
    }

    /// Take up to `wanted` bytes; returns how many were granted.
    fn take(&self, wanted: usize) -> usize {
        let before = self.remaining.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| Some(left.saturating_sub(wanted)));
        before.unwrap_or(0).min(wanted)
    }

    fn spent(&self) -> bool {
        self.remaining.load(Ordering::SeqCst) == 0
    }

    /// True for exactly one caller: whichever stream reaches the cap first writes the note.
    fn first_to_note(&self) -> bool {
        !self.noted.swap(true, Ordering::SeqCst)
    }
}

/// One pipe of a streamed command, decoded and sent on as events.
struct Forward {
    pipe: Pipe,
    text: Utf8Decoder,
    budget: Arc<OutputBudget>,
    events: tokio::sync::mpsc::UnboundedSender<ExecEvent>,
}

impl Forward {
    fn new(pipe: Pipe, budget: Arc<OutputBudget>, events: tokio::sync::mpsc::UnboundedSender<ExecEvent>) -> Self {
        Forward { pipe, text: Utf8Decoder::default(), budget, events }
    }

    fn send(&self, pipe: Pipe, data: String) {
        let event = match pipe {
            Pipe::Stdout => ExecEvent::Stdout { data },
            Pipe::Stderr => ExecEvent::Stderr { data },
        };
        // A send fails only once the stream is dropped, and then the command is being stopped.
        let _ = self.events.send(event);
    }

    fn forward(&mut self, mut text: String) {
        if text.is_empty() {
            return;
        }
        let granted = self.budget.take(text.len());
        if granted < text.len() {
            let mut cut = granted;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            if !text.is_empty() {
                self.send(self.pipe, text);
            }
            self.note();
            return;
        }
        self.send(self.pipe, text);
    }

    fn note(&self) {
        if self.budget.first_to_note() {
            let cap = EXEC_STREAM_OUTPUT_CAP / (1024 * 1024);
            self.send(Pipe::Stderr, format!("\n… [more than {cap} MiB of output; the rest is not shown] …\n"));
        }
    }
}

impl Sink for Forward {
    fn push(&mut self, bytes: &[u8]) {
        // Past the cap the pipe is still read — a command blocked on a full pipe would never
        // finish — but nothing more is sent.
        if self.budget.spent() {
            self.note();
            return;
        }
        let text = self.text.push(bytes);
        self.forward(text);
    }

    fn end(&mut self) {
        if !self.budget.spent() {
            let text = self.text.finish();
            self.forward(text);
        }
    }
}

/// **Bytes to text, one read at a time.** A character split between two reads is held until its
/// last byte arrives; a byte that can never be part of UTF-8 becomes U+FFFD, as
/// `String::from_utf8_lossy` would make it.
#[derive(Default)]
struct Utf8Decoder {
    /// The start of a character whose end has not been read yet: at most three bytes.
    partial: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        let mut input = std::mem::take(&mut self.partial);
        input.extend_from_slice(bytes);
        let mut text = String::with_capacity(input.len());
        let mut rest = input.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match error.error_len() {
                        Some(invalid) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[invalid..];
                        }
                        // Not wrong, only unfinished: the rest of it is in the next read.
                        None => {
                            self.partial = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        text
    }

    /// The pipe closed: a character still unfinished never will be.
    fn finish(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::open(dir.path().to_str().unwrap()).expect("opens");
        (dir, workspace)
    }

    fn put(workspace: &Workspace, rel: &str, content: &str) {
        let path = workspace.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn message(error: Error) -> String {
        error.to_string()
    }

    // --- path safety ---------------------------------------------------------------------

    #[test]
    fn a_root_must_be_an_existing_absolute_directory() {
        assert!(Workspace::open("").is_err());
        assert!(message(Workspace::open("relative/dir").unwrap_err()).contains("absolute"));
        assert!(Workspace::open("/definitely/not/here/misaka").is_err());
        let (dir, _) = workspace();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(message(Workspace::open(file.to_str().unwrap()).unwrap_err()).contains("not a directory"));
    }

    #[test]
    fn relative_paths_resolve_inside_the_root() {
        let (_dir, ws) = workspace();
        put(&ws, "src/main.rs", "fn main() {}\n");
        let resolved = ws.resolve("src/../src/./main.rs").unwrap();
        assert_eq!(resolved.rel, "src/main.rs");
        assert!(resolved.exists);
        assert_eq!(resolved.real, ws.root().join("src/main.rs"));
        assert_eq!(ws.resolve("").unwrap().rel, ".");
        assert_eq!(ws.resolve(".").unwrap().rel, ".");
        let missing = ws.resolve("new/dir/file.txt").unwrap();
        assert!(!missing.exists);
        assert_eq!(missing.real, ws.root().join("new/dir/file.txt"));
    }

    #[test]
    fn dot_dot_cannot_climb_out() {
        let (_dir, ws) = workspace();
        for escape in ["..", "../x", "a/../../x", "./../../etc/passwd"] {
            let error = message(ws.resolve(escape).unwrap_err());
            assert!(error.contains("path escapes the workspace"), "{escape}: {error}");
        }
    }

    #[test]
    fn absolute_paths_are_accepted_only_inside_the_root() {
        let (_dir, ws) = workspace();
        put(&ws, "src/lib.rs", "");
        let inside = ws.root().join("src/lib.rs");
        assert_eq!(ws.resolve(inside.to_str().unwrap()).unwrap().rel, "src/lib.rs");
        let error = message(ws.resolve("/etc/passwd").unwrap_err());
        assert!(error.contains("path escapes the workspace"), "{error}");
        // `/src/lib.rs` is a workspace path spelled with a leading slash, and the error says so.
        let error = message(ws.resolve("/src/lib.rs").unwrap_err());
        assert!(error.contains("did you mean src/lib.rs"), "{error}");
        // `..` inside an absolute path is resolved before the check.
        let sneaky = format!("{}/../outside", ws.root().display());
        assert!(ws.resolve(&sneaky).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_refused_however_innocent_the_name() {
        let (_dir, ws) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.root().join("docs")).unwrap();
        let error = message(ws.resolve("docs/secret").unwrap_err());
        assert!(error.contains("path escapes the workspace"), "{error}");
        // A file that does not exist yet behind the link is refused too: its nearest existing
        // ancestor is outside.
        assert!(ws.resolve("docs/new.txt").is_err());
        assert!(write(&ws, "docs/new.txt", "x".into()).is_err());
        assert!(!outside.path().join("new.txt").exists(), "nothing was written outside");
        // A link that stays inside is fine.
        put(&ws, "real/a.txt", "a");
        std::os::unix::fs::symlink(ws.root().join("real"), ws.root().join("alias")).unwrap();
        assert_eq!(ws.resolve("alias/a.txt").unwrap().rel, "alias/a.txt");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_cannot_be_written_through() {
        let (_dir, ws) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("not-yet");
        std::os::unix::fs::symlink(&target, ws.root().join("link")).unwrap();
        // The link itself is replaced (rename does not follow it); the outside path never appears.
        let _ = write(&ws, "link", "x".into());
        assert!(!target.exists(), "a write never follows a link out of the workspace");
        let _ = write(&ws, "link/inner.txt", "x".into());
        assert!(!outside.path().join("not-yet/inner.txt").exists());
    }

    #[test]
    fn nothing_inside_dot_git_changes() {
        let (_dir, ws) = workspace();
        put(&ws, ".git/config", "[core]\n");
        put(&ws, ".git/HEAD", "ref: refs/heads/main\n");
        for path in [".git/config", ".git/hooks/pre-commit", "sub/.git/x", ".GIT/config"] {
            let error = message(write(&ws, path, "x".into()).unwrap_err());
            assert!(error.contains(".git"), "{path}: {error}");
        }
        assert!(edit(&ws, ".git/config", "[core]", "[x]", false).is_err());
        assert!(remove(&ws, ".git/HEAD").is_err());
        assert_eq!(std::fs::read_to_string(ws.root().join(".git/config")).unwrap(), "[core]\n");
        // Reading it is fine.
        assert_eq!(read(&ws, ".git/HEAD", None, None, None).unwrap().content, "ref: refs/heads/main\n");
        // And `.github` is not `.git`.
        assert!(write(&ws, ".github/workflows/ci.yml", "on: push\n".into()).is_ok());
    }

    // --- open / browse ---------------------------------------------------------------------

    #[test]
    fn open_reads_the_branch_without_git() {
        let (_dir, ws) = workspace();
        put(&ws, ".git/HEAD", "ref: refs/heads/feature/agent\n");
        let info = open(ws.root().to_str().unwrap()).unwrap();
        assert!(info.is_git);
        assert_eq!(info.branch.as_deref(), Some("feature/agent"));
        assert_eq!(info.name, ws.root().file_name().unwrap().to_string_lossy());

        put(&ws, ".git/HEAD", "0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(open(ws.root().to_str().unwrap()).unwrap().branch.as_deref(), Some("0123456"), "detached: the short sha");

        // A subfolder of a repository is in that repository.
        put(&ws, "crates/a/x.rs", "");
        let sub = open(ws.root().join("crates/a").to_str().unwrap()).unwrap();
        assert!(sub.is_git);
    }

    #[test]
    fn a_worktrees_git_file_is_followed() {
        let (_dir, ws) = workspace();
        put(&ws, "main/.git/worktrees/wt/HEAD", "ref: refs/heads/wt-branch\n");
        put(&ws, "wt/.git", "gitdir: ../main/.git/worktrees/wt\n");
        let info = open(ws.root().join("wt").to_str().unwrap()).unwrap();
        assert!(info.is_git);
        assert_eq!(info.branch.as_deref(), Some("wt-branch"));
    }

    #[test]
    fn browse_lists_subdirectories_only_sorted_and_without_dot_dirs() {
        let (_dir, ws) = workspace();
        for d in ["beta", "Alpha", "gamma/.git", ".hidden"] {
            std::fs::create_dir_all(ws.root().join(d)).unwrap();
        }
        put(&ws, "file.txt", "");
        let listing = browse(Some(ws.root().to_str().unwrap()), false).unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "gamma"]);
        assert!(listing.entries.iter().all(|e| e.is_dir));
        assert!(listing.entries.iter().find(|e| e.name == "gamma").unwrap().is_git);
        assert!(!listing.entries.iter().find(|e| e.name == "beta").unwrap().is_git);
        assert!(listing.parent.is_some());
        let with_hidden = browse(Some(ws.root().to_str().unwrap()), true).unwrap();
        assert!(with_hidden.entries.iter().any(|e| e.name == ".hidden"));
        assert!(browse(Some("relative"), false).is_err());
        let root = browse(Some("/"), false).unwrap();
        assert_eq!(root.parent, None);
    }

    // --- tree / files ----------------------------------------------------------------------

    #[test]
    fn the_tree_honours_gitignore_keeps_dotfiles_and_never_lists_dot_git() {
        let (_dir, ws) = workspace();
        put(&ws, ".git/HEAD", "ref: refs/heads/main\n");
        put(&ws, ".gitignore", "ignored/\n*.log\n");
        put(&ws, ".github/workflows/ci.yml", "");
        put(&ws, "ignored/a.txt", "");
        put(&ws, "debug.log", "");
        put(&ws, "src/main.rs", "fn main() {}\n");
        put(&ws, "build/keep.txt", "");
        put(&ws, "README.md", "");
        let listing = tree(&ws, None, Some(3), None).unwrap();
        let paths: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                ".github",
                ".github/workflows",
                ".github/workflows/ci.yml",
                "build",
                "build/keep.txt",
                "src",
                "src/main.rs",
                ".gitignore",
                "README.md"
            ],
            "explorer order: directories first at every level; a tracked build/ is listed when a .gitignore governs"
        );
        assert!(!listing.truncated);
        let main = listing.entries.iter().find(|e| e.path == "src/main.rs").unwrap();
        assert_eq!(main.size, Some(13));
        assert_eq!(listing.entries.iter().find(|e| e.path == "src").unwrap().size, None);
    }

    #[test]
    fn without_a_gitignore_the_heavy_directories_are_skipped() {
        let (_dir, ws) = workspace();
        put(&ws, "node_modules/x/index.js", "");
        put(&ws, "target/debug/app", "");
        put(&ws, "__pycache__/m.pyc", "");
        put(&ws, "app/main.py", "");
        let listing = tree(&ws, None, Some(4), None).unwrap();
        let paths: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["app", "app/main.py"]);
        // Asked for by name, a heavy directory is still listed.
        let inside = tree(&ws, Some("node_modules"), Some(2), None).unwrap();
        assert_eq!(inside.path, "node_modules");
        assert!(inside.entries.iter().any(|e| e.path == "node_modules/x/index.js"));
    }

    #[test]
    fn a_tree_over_its_limit_keeps_the_shallow_entries() {
        let (_dir, ws) = workspace();
        for i in 0..5 {
            put(&ws, &format!("a/deep{i}.txt"), "");
        }
        put(&ws, "z.txt", "");
        let listing = tree(&ws, None, Some(2), Some(3)).unwrap();
        assert!(listing.truncated);
        let paths: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"a") && paths.contains(&"z.txt"), "the whole top level first: {paths:?}");
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn the_fuzzy_finder_prefers_names_word_starts_and_short_paths() {
        let (_dir, ws) = workspace();
        for p in ["src/workspace.rs", "src/api/workspace.rs", "docs/walkthrough-scripts.md", "src/main.rs", "tests/main_test.rs"] {
            put(&ws, p, "");
        }
        let found = files(&ws, "workspace", None).unwrap().paths;
        assert_eq!(found[0], "src/workspace.rs", "{found:?}");
        assert_eq!(found[1], "src/api/workspace.rs");
        let found = files(&ws, "main", None).unwrap().paths;
        assert_eq!(found[0], "src/main.rs", "{found:?}");
        let found = files(&ws, "apiws", None).unwrap().paths;
        assert_eq!(found[0], "src/api/workspace.rs", "{found:?}");
        assert!(files(&ws, "qqqq", None).unwrap().paths.is_empty());
        assert_eq!(files(&ws, "", Some(2)).unwrap().paths.len(), 2);
    }

    #[test]
    fn an_empty_query_lists_the_newest_files_first() {
        let (_dir, ws) = workspace();
        put(&ws, "old.txt", "");
        let old = std::fs::File::options().write(true).open(ws.root().join("old.txt")).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
        put(&ws, "new.txt", "");
        assert_eq!(files(&ws, "", None).unwrap().paths, vec!["new.txt", "old.txt"]);
    }

    #[test]
    fn subsequence_scoring_rewards_boundaries_and_runs() {
        let q: Vec<char> = "wsrs".chars().collect();
        let boundary = subsequence_score(&q, &"workspace_rs".chars().collect::<Vec<_>>()).unwrap();
        let q2: Vec<char> = "rs".chars().collect();
        let run = subsequence_score(&q2, &"x.rs".chars().collect::<Vec<_>>()).unwrap();
        let spread = subsequence_score(&q2, &"rxxs".chars().collect::<Vec<_>>()).unwrap();
        assert!(run > spread, "a run beats a gap: {run} vs {spread}");
        assert!(boundary > 0);
        assert_eq!(subsequence_score(&q, &"abc".chars().collect::<Vec<_>>()), None);
    }

    // --- read ------------------------------------------------------------------------------

    #[test]
    fn read_returns_the_requested_lines_raw() {
        let (_dir, ws) = workspace();
        put(&ws, "a.txt", "one\ntwo\r\nthree\nfour");
        let all = read(&ws, "a.txt", None, None, None).unwrap();
        assert_eq!(all.content, "one\ntwo\r\nthree\nfour");
        assert_eq!((all.start_line, all.end_line, all.total_lines), (1, 4, 4));
        assert!(!all.truncated && !all.binary);
        let middle = read(&ws, "a.txt", Some(2), Some(3), None).unwrap();
        assert_eq!(middle.content, "two\r\nthree\n");
        assert_eq!((middle.start_line, middle.end_line), (2, 3));
        let clamped = read(&ws, "a.txt", Some(3), Some(99), None).unwrap();
        assert_eq!(clamped.content, "three\nfour");
        assert_eq!(clamped.end_line, 4);
        assert!(message(read(&ws, "a.txt", Some(9), None, None).unwrap_err()).contains("past the end"));
        assert!(message(read(&ws, "a.txt", Some(3), Some(2), None).unwrap_err()).contains("before start_line"));
        assert!(read(&ws, "missing.txt", None, None, None).is_err());
        assert!(message(read(&ws, ".", None, None, None).unwrap_err()).contains("directory"));
    }

    #[test]
    fn read_truncates_at_a_line_boundary() {
        let (_dir, ws) = workspace();
        put(&ws, "a.txt", "aaaa\nbbbb\ncccc\n");
        let cut = read(&ws, "a.txt", None, None, Some(12)).unwrap();
        assert_eq!(cut.content, "aaaa\nbbbb\n");
        assert_eq!(cut.end_line, 2);
        assert_eq!(cut.total_lines, 3, "every line is still counted");
        assert!(cut.truncated);
        // One line longer than the budget comes back cut on a character boundary.
        put(&ws, "long.txt", &"é".repeat(10));
        let long = read(&ws, "long.txt", None, None, Some(5)).unwrap();
        assert_eq!(long.content, "éé");
        assert!(long.truncated);
    }

    #[test]
    fn read_reports_binary_and_empty_files() {
        let (_dir, ws) = workspace();
        std::fs::write(ws.root().join("b.bin"), [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
        let binary = read(&ws, "b.bin", None, None, None).unwrap();
        assert!(binary.binary);
        assert_eq!(binary.content, "");
        assert_eq!(binary.size, 7);
        put(&ws, "empty.txt", "");
        let empty = read(&ws, "empty.txt", None, None, None).unwrap();
        assert_eq!((empty.start_line, empty.end_line, empty.total_lines), (0, 0, 0));
    }

    // --- search ----------------------------------------------------------------------------

    fn search_for(ws: &Workspace, pattern: &str, configure: impl FnOnce(&mut SearchRequest)) -> Result<SearchResponse> {
        let mut request = SearchRequest { pattern: pattern.into(), regex: true, ..Default::default() };
        configure(&mut request);
        search(ws, &request)
    }

    #[test]
    fn search_finds_lines_like_ripgrep() {
        let (_dir, ws) = workspace();
        put(&ws, ".gitignore", "skipped/\n");
        put(&ws, "src/a.rs", "fn alpha() {}\nfn beta() {}\n");
        put(&ws, "src/b.ts", "export function alpha() {}\r\n");
        put(&ws, "skipped/c.rs", "fn alpha() {}\n");
        std::fs::write(ws.root().join("bin.dat"), b"alpha\0").unwrap();
        let found = search_for(&ws, r"fn \w+\(", |_| {}).unwrap();
        assert_eq!(found.matches.len(), 2);
        assert_eq!(found.matches[0], SearchMatch { path: "src/a.rs".into(), line: 1, text: "fn alpha() {}".into() });
        let anchored = search_for(&ws, r"\(\) \{\}$", |_| {}).unwrap();
        assert_eq!(anchored.matches.len(), 3, "`$` is the line's end, CRLF included: {:?}", anchored.matches);
        let rs_only = search_for(&ws, "alpha", |r| r.glob = Some("*.rs".into())).unwrap();
        assert_eq!(rs_only.matches.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), vec!["src/a.rs"]);
        let not_rs = search_for(&ws, "alpha", |r| r.glob = Some("!*.rs".into())).unwrap();
        assert_eq!(not_rs.matches.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(), vec!["src/b.ts"]);
        let nested = search_for(&ws, "alpha", |r| r.glob = Some("src/**/*.{ts,tsx}".into())).unwrap();
        assert_eq!(nested.matches.len(), 1);
        let literal = search_for(&ws, "alpha()", |r| r.regex = false).unwrap();
        assert_eq!(literal.matches.len(), 2);
        let insensitive = search_for(&ws, "ALPHA", |r| r.case_insensitive = true).unwrap();
        assert_eq!(insensitive.matches.len(), 2);
        let within = search_for(&ws, "alpha", |r| r.path = Some("src/b.ts".into())).unwrap();
        assert_eq!(within.matches.len(), 1);
        assert_eq!(within.files_scanned, 1);
        let error = message(search_for(&ws, "alpha(", |_| {}).unwrap_err());
        assert!(error.contains("invalid regex") && error.contains("regex: false"), "{error}");
    }

    #[test]
    fn search_stops_at_max_results_and_says_so() {
        let (_dir, ws) = workspace();
        put(&ws, "many.txt", &"hit\n".repeat(20));
        let found = search_for(&ws, "hit", |r| r.max_results = Some(5)).unwrap();
        assert_eq!(found.matches.len(), 5);
        assert!(found.truncated);
        let all = search_for(&ws, "hit", |r| r.max_results = Some(20)).unwrap();
        assert!(!all.truncated);
    }

    #[test]
    fn a_long_line_is_cut_around_its_match() {
        let regex = regex::bytes::Regex::new("NEEDLE").unwrap();
        let line = format!("{}NEEDLE{}", "a".repeat(1000), "b".repeat(1000));
        let text = excerpt(&line, &regex);
        assert!(text.chars().count() <= SEARCH_LINE_CHARS, "{}", text.chars().count());
        assert!(text.contains("NEEDLE"));
        assert!(text.starts_with('…') && text.ends_with('…'));
    }

    #[test]
    fn globs_split_outside_braces() {
        assert_eq!(split_globs("*.{ts,tsx}, !*.d.ts src/**"), vec!["*.{ts,tsx}", "!*.d.ts", "src/**"]);
    }

    // --- write / edit / remove -------------------------------------------------------------

    #[test]
    fn write_creates_folders_and_reports_what_was_there() {
        let (_dir, ws) = workspace();
        let created = write(&ws, "a/b/c.txt", "hello\n".into()).unwrap();
        assert!(created.created);
        assert_eq!(created.previous, None);
        assert_eq!(created.bytes, 6);
        let replaced = write(&ws, "a/b/c.txt", "bye\n".into()).unwrap();
        assert!(!replaced.created);
        assert_eq!(replaced.previous.as_deref(), Some("hello\n"));
        assert_eq!(std::fs::read_to_string(ws.root().join("a/b/c.txt")).unwrap(), "bye\n");
        let leftovers: Vec<_> = std::fs::read_dir(ws.root().join("a/b")).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left behind");
        assert!(message(write(&ws, "a", "x".into()).unwrap_err()).contains("directory"));
        assert!(write(&ws, "big.txt", "x".repeat(WRITE_MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn a_binary_file_is_never_overwritten() {
        let (_dir, ws) = workspace();
        std::fs::write(ws.root().join("img.png"), [0x89, b'P', 0, 0]).unwrap();
        assert!(message(write(&ws, "img.png", "text".into()).unwrap_err()).contains("binary"));
        assert_eq!(std::fs::read(ws.root().join("img.png")).unwrap(), vec![0x89, b'P', 0, 0]);
    }

    #[cfg(unix)]
    #[test]
    fn write_and_edit_keep_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, ws) = workspace();
        put(&ws, "run.sh", "#!/bin/sh\necho hi\n");
        std::fs::set_permissions(ws.root().join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&ws, "run.sh", "#!/bin/sh\necho bye\n".into()).unwrap();
        edit(&ws, "run.sh", "bye", "ciao", false).unwrap();
        let mode = std::fs::metadata(ws.root().join("run.sh")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        std::fs::set_permissions(ws.root().join("run.sh"), std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(message(write(&ws, "run.sh", "x".into()).unwrap_err()).contains("read-only"));
    }

    #[test]
    fn edit_replaces_one_exact_match() {
        let (_dir, ws) = workspace();
        put(&ws, "a.rs", "let x = 1;\nlet y = 2;\n");
        let done = edit(&ws, "a.rs", "let y = 2;", "let y = 3;", false).unwrap();
        assert_eq!(done.replacements, 1);
        assert_eq!(done.previous, "let x = 1;\nlet y = 2;\n");
        assert_eq!(done.content, "let x = 1;\nlet y = 3;\n");
        assert_eq!(std::fs::read_to_string(ws.root().join("a.rs")).unwrap(), done.content);
    }

    #[test]
    fn edit_errors_are_instructions() {
        let content = "a = 1\nb = 1\n";
        let missing = message(apply_edit(content, "c = 1", "c = 2", false, "x.py").unwrap_err());
        assert_eq!(missing, "old_string was not found in x.py. Read the file again and copy the exact text, including whitespace.");
        let many = message(apply_edit(content, "= 1", "= 2", false, "x.py").unwrap_err());
        assert_eq!(many, "old_string matches 2 places in x.py; include more surrounding lines to make it unique, or set replace_all");
        let all = apply_edit(content, "= 1", "= 2", true, "x.py").unwrap();
        assert_eq!((all.content.as_str(), all.replacements), ("a = 2\nb = 2\n", 2));
        assert!(message(apply_edit(content, "a", "a", false, "x.py").unwrap_err()).contains("identical"));
        assert!(message(apply_edit(content, "", "z", false, "x.py").unwrap_err()).contains("empty"));
        // Overlapping occurrences are ambiguous too.
        assert!(message(apply_edit("aaa", "aa", "b", false, "x").unwrap_err()).contains("matches 2 places"));
    }

    #[test]
    fn edit_forgives_line_endings() {
        // The file is CRLF; the model wrote LF.
        let crlf = "fn a() {\r\n    1\r\n}\r\nfn b() {}\r\n";
        let done = apply_edit(crlf, "fn a() {\n    1\n}", "fn a() {\n    2\n}", false, "x.rs").unwrap();
        assert_eq!(done.content, "fn a() {\r\n    2\r\n}\r\nfn b() {}\r\n", "and new_string is written in CRLF");
        // The file is LF; the model wrote CRLF.
        let lf = "x\ny\nz\n";
        let done = apply_edit(lf, "x\r\ny\r\n", "x\r\nY\r\n", false, "x").unwrap();
        assert_eq!(done.content, "x\nY\nz\n");
    }

    #[test]
    fn edit_forgives_trailing_whitespace_but_not_indentation() {
        // The file has trailing spaces the model dropped.
        let file = "def f():   \n    return 1  \n\nprint(f())\n";
        let done = apply_edit(file, "def f():\n    return 1\n", "def f():\n    return 2\n", false, "f.py").unwrap();
        assert_eq!(done.content, "def f():\n    return 2\n\nprint(f())\n");
        // The model added trailing spaces the file does not have, on its last line too.
        let file = "alpha\nbeta\ngamma\n";
        let done = apply_edit(file, "alpha  \nbeta  ", "ALPHA\nBETA", false, "g").unwrap();
        assert_eq!(done.content, "ALPHA\nBETA\ngamma\n");
        // Mixed: a CRLF file with trailing whitespace, edited from LF text.
        let file = "one \r\ntwo\r\n";
        let done = apply_edit(file, "one\ntwo\n", "uno\ndos\n", false, "m").unwrap();
        assert_eq!(done.content, "uno\r\ndos\r\n");
        // A needle ending in a newline matches a last line that has none.
        let file = "first\nlast";
        let done = apply_edit(file, "last\n", "LAST\n", false, "e").unwrap();
        assert_eq!(done.content, "first\nLAST\n");
        // Indentation is never forgiven, but the error says where.
        let file = "if x:\n\treturn 1\n";
        let error = message(apply_edit(file, "if x:\n    return 1", "if x:\n    return 2", false, "i.py").unwrap_err());
        assert!(error.contains("starts at line 1"), "{error}");
        // Forgiving whitespace does not invent uniqueness.
        let file = "a  \nb\na \nb\n";
        assert!(message(apply_edit(file, "a\nb\n", "c\n", false, "u").unwrap_err()).contains("matches 2 places"));
        let all = apply_edit(file, "a\nb\n", "c\n", true, "u").unwrap();
        assert_eq!((all.content.as_str(), all.replacements), ("c\nc\n", 2));
        // And an exact match is preferred over a loose one.
        let file = "a  \nb\na\nb\n";
        assert_eq!(apply_edit(file, "a\nb\n", "c\n", false, "u").unwrap().content, "a  \nb\nc\n");
    }

    #[test]
    fn a_mid_line_needle_keeps_its_meaningful_trailing_space() {
        // The first line differs only in trailing spaces, so only the loose comparison matches —
        // and the needle's last line ends mid-line with a space that tells the two candidates
        // apart. Trimming it would match `foo\nbar` twice.
        let file = "foo  \nbar baz\nfoo\nbarbaz\n";
        let done = apply_edit(file, "foo\nbar ", "FOO\nBAR ", false, "t").unwrap();
        assert_eq!(done.content, "FOO\nBAR baz\nfoo\nbarbaz\n");
    }

    #[test]
    fn edit_refuses_what_it_cannot_rewrite() {
        let (_dir, ws) = workspace();
        assert!(message(edit(&ws, "nope.txt", "a", "b", false).unwrap_err()).contains("use write"));
        std::fs::write(ws.root().join("latin1.txt"), [b'c', b'a', b'f', 0xE9, b'\n']).unwrap();
        assert!(message(edit(&ws, "latin1.txt", "caf", "cof", false).unwrap_err()).contains("UTF-8"));
    }

    #[test]
    fn remove_deletes_files_only() {
        let (_dir, ws) = workspace();
        put(&ws, "made/by/agent.txt", "x");
        assert_eq!(remove(&ws, "made/by/agent.txt").unwrap().removed, "made/by/agent.txt");
        assert!(!ws.root().join("made/by/agent.txt").exists());
        assert!(message(remove(&ws, "made").unwrap_err()).contains("directory"));
        assert!(message(remove(&ws, "made/by/agent.txt").unwrap_err()).contains("does not exist"));
        assert!(remove(&ws, ".").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_symlink_removes_the_link_not_its_target() {
        let (_dir, ws) = workspace();
        put(&ws, "target.txt", "keep");
        std::os::unix::fs::symlink(ws.root().join("target.txt"), ws.root().join("link.txt")).unwrap();
        remove(&ws, "link.txt").unwrap();
        assert!(std::fs::symlink_metadata(ws.root().join("link.txt")).is_err());
        assert_eq!(std::fs::read_to_string(ws.root().join("target.txt")).unwrap(), "keep");
    }

    // --- exec ------------------------------------------------------------------------------

    #[test]
    fn a_capture_keeps_both_ends() {
        let mut capture = Capture::default();
        capture.push(b"small");
        assert_eq!(capture.finish(), ("small".to_string(), false));

        let mut capture = Capture::default();
        let total = EXEC_STREAM_CAP * 3;
        for chunk in vec![b'x'; total].chunks(7000) {
            capture.push(chunk);
        }
        capture.push(b"THE END");
        let (text, cut) = capture.finish();
        assert!(cut);
        assert!(text.ends_with("THE END"));
        assert!(text.contains(&format!("… [{} bytes omitted] …", total + 7 - EXEC_STREAM_CAP)), "{}", &text[49_000..52_000]);
        assert!(text.len() < EXEC_STREAM_CAP + 64);
    }

    /// Line output is cut between lines, never through a number.
    #[test]
    fn a_cut_falls_between_lines() {
        let mut capture = Capture::default();
        let lines: String = (1..=60_000).map(|n| format!("{n}\n")).collect();
        capture.push(lines.as_bytes());
        let (text, cut) = capture.finish();
        assert!(cut);
        let (head, rest) = text.split_once("\n… [").expect("a marker");
        let (marker, tail) = rest.split_once("] …\n").expect("a whole marker");
        let numbers = |s: &str| s.lines().map(|l| l.parse::<u32>().expect("every kept line is whole")).collect::<Vec<_>>();
        let (head, tail) = (numbers(head), numbers(tail));
        assert_eq!(head.first(), Some(&1));
        assert_eq!(tail.last(), Some(&60_000));
        assert!(tail[0] > *head.last().unwrap() + 1, "the middle is gone");
        let omitted: usize = marker.trim_end_matches(" bytes omitted").parse().unwrap();
        let kept: usize = head.iter().chain(&tail).map(|n| n.to_string().len() + 1).sum();
        assert_eq!(omitted + kept, lines.len(), "the count is exact");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_runs_in_the_root_with_the_agent_environment() {
        let (_dir, ws) = workspace();
        let out = exec(&ws, "pwd; echo \"$MISAKA_AGENT $PAGER $TERM\"; echo oops >&2; exit 3", None).await.unwrap();
        assert_eq!(out.exit_code, Some(3));
        assert!(!out.timed_out);
        let mut lines = out.stdout.lines();
        assert_eq!(std::fs::canonicalize(lines.next().unwrap()).unwrap(), ws.root());
        assert_eq!(lines.next(), Some("1 cat dumb"));
        assert_eq!(out.stderr.trim(), "oops");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timeout_kills_the_whole_group() {
        let (_dir, ws) = workspace();
        let marker = ws.root().join("survived");
        // A background child that would write the marker after the timeout if it lived.
        let command = format!("(sleep 3; touch '{}') & sleep 30", marker.display());
        let started = Instant::now();
        let out = exec(&ws, &command, Some(1)).await.unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "the background child was killed with its group");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_has_no_terminal_and_no_stdin() {
        let (_dir, ws) = workspace();
        let out = exec(&ws, "cat; echo done; (exec </dev/tty) 2>/dev/null && echo tty || echo no-tty", Some(10)).await.unwrap();
        assert_eq!(out.stdout.lines().collect::<Vec<_>>(), vec!["done", "no-tty"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn huge_output_is_cut_in_the_middle() {
        let (_dir, ws) = workspace();
        let out = exec(&ws, "yes line | head -n 100000; echo LAST", Some(30)).await.unwrap();
        assert!(out.truncated);
        assert!(out.stdout.starts_with("line\n"));
        assert!(out.stdout.trim_end().ends_with("LAST"));
        assert!(out.stdout.contains("bytes omitted"));
    }

    // --- exec_stream ---------------------------------------------------------------------------

    #[test]
    fn a_character_split_between_reads_arrives_whole() {
        let mut decoder = Utf8Decoder::default();
        let text = "aあ😀b".as_bytes();
        // Every way of cutting it in two decodes to the same text, with nothing replaced.
        for cut in 0..=text.len() {
            let mut decoded = decoder.push(&text[..cut]);
            decoded.push_str(&decoder.push(&text[cut..]));
            decoded.push_str(&decoder.finish());
            assert_eq!(decoded, "aあ😀b", "cut at {cut}");
        }
        // One byte at a time, too.
        let decoded: String = text.iter().map(|b| decoder.push(&[*b])).collect();
        assert_eq!(decoded, "aあ😀b");
        // Bytes that can never be UTF-8 are replaced as `from_utf8_lossy` replaces them, and an
        // unfinished character at the very end is replaced when the pipe closes.
        assert_eq!(decoder.push(b"x\xffy\xe3\x81"), "x\u{FFFD}y");
        assert_eq!(decoder.finish(), "\u{FFFD}");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn the_output_budget_is_shared_and_noted_once() {
        let budget = OutputBudget::new(10);
        assert_eq!(budget.take(4), 4);
        assert_eq!(budget.take(4), 4);
        assert_eq!(budget.take(4), 2, "only what is left");
        assert!(budget.spent());
        assert_eq!(budget.take(1), 0);
        assert!(budget.first_to_note());
        assert!(!budget.first_to_note());
    }

    #[cfg(unix)]
    async fn stream_events(ws: &Workspace, command: &str, timeout_secs: Option<u64>) -> Vec<ExecEvent> {
        use futures_util::StreamExt;
        exec_stream(ws, command, timeout_secs).unwrap().collect().await
    }

    /// What a run printed on one pipe, events joined.
    #[cfg(unix)]
    fn printed(events: &[ExecEvent], stdout: bool) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ExecEvent::Stdout { data } if stdout => Some(data.as_str()),
                ExecEvent::Stderr { data } if !stdout => Some(data.as_str()),
                _ => None,
            })
            .collect()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_streamed_command_reports_both_pipes_then_its_exit() {
        let (_dir, ws) = workspace();
        // `あ` is E3 81 82: its last byte is written a moment after the first two.
        let events =
            stream_events(&ws, r"printf 'out\n'; printf 'err\n' >&2; printf '\343\201'; sleep 0.3; printf '\202\n'; exit 3", None)
                .await;
        assert_eq!(printed(&events, true), "out\nあ\n");
        assert_eq!(printed(&events, false), "err\n");
        let ExecEvent::Exit { exit_code, timed_out, duration_ms } = events.last().unwrap().clone() else { panic!("{events:?}") };
        assert_eq!((exit_code, timed_out), (Some(3), false));
        assert!(duration_ms >= 300, "{duration_ms}");
        assert_eq!(events.iter().filter(|e| matches!(e, ExecEvent::Exit { .. })).count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streamed_output_stops_at_the_cap_and_the_command_still_finishes() {
        let (_dir, ws) = workspace();
        let command = format!("head -c {} /dev/zero | tr '\\0' x; echo after-the-cap >&2", EXEC_STREAM_OUTPUT_CAP + 1024 * 1024);
        let events = stream_events(&ws, &command, Some(60)).await;
        let stdout = printed(&events, true);
        assert_eq!(stdout.len(), EXEC_STREAM_OUTPUT_CAP);
        assert!(stdout.bytes().all(|b| b == b'x'));
        let stderr = printed(&events, false);
        assert_eq!(stderr.matches("the rest is not shown").count(), 1, "{stderr}");
        assert!(!stderr.contains("after-the-cap"), "the cap covers both pipes");
        assert!(matches!(events.last(), Some(ExecEvent::Exit { exit_code: Some(0), timed_out: false, .. })), "{:?}", events.last());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_command_is_refused_before_anything_starts() {
        let (_dir, ws) = workspace();
        assert!(exec_stream(&ws, "  ", None).is_err());
    }
}
