//! **The workspace as git sees it: what changed, and the diff of it.** Read-only, always.
//!
//! git is run directly — never through a shell, so no file name can be read as shell syntax — as
//! `git -C <root>`, with everything that would change what it prints pinned on its command line:
//!
//! * `GIT_OPTIONAL_LOCKS=0`. `git status` normally refreshes the index and writes it back, taking
//!   `index.lock` to do it. Asked every few seconds by a UI while the person runs their own
//!   `git commit` in a terminal, that lock is a collision ("Unable to create index.lock: File
//!   exists") they did nothing to cause. Without it, status only reads.
//! * Porcelain v2 and `-z`: a format git promises to keep, NUL-separated, so a name with spaces,
//!   quotes or a newline in it arrives as it is.
//! * No pager, no colour, no external diff tool, no textconv driver, `a/` and `b/` prefixes
//!   whatever `diff.noprefix` says, and names unquoted (`core.quotepath=false`).
//! * `core.fsmonitor=false`: a repository's config may name an fsmonitor *hook*, a program every
//!   `git status` runs. Opening a folder must not be what runs one.
//!
//! Paths come back relative to the WORKSPACE root, which may be a subdirectory of the repository:
//! git is asked about that directory only (`-- .`), and its repository-relative names are made
//! root-relative here.

use super::{ProcessGroup, Workspace, bad, detach, display_path, find_git, looks_binary, sniff};
use crate::{Error, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// How long one git command may run before it is stopped.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// `diff`: the most patch returned. Past it the patch is cut at a line boundary.
pub const DIFF_MAX_BYTES: usize = 1024 * 1024;
/// `diff` of a folder: new files appended to the patch, at most. Each one is a git run.
pub const DIFF_MAX_NEW_FILES: usize = 100;
/// `status`: a new file larger than this is not read to count its lines (`added: null`).
pub const COUNT_MAX_FILE_BYTES: u64 = 1024 * 1024;
/// `status`: how much of the new files is read to count their lines, in all, and how many are
/// counted. Past either the rest are `null` — a folder whose `node_modules` is not ignored must
/// still answer in a moment.
const COUNT_BUDGET_BYTES: u64 = 32 * 1024 * 1024;
const COUNT_BUDGET_FILES: usize = 5000;
/// What a listing (`status`, `--numstat`) may print before it is refused as too large to use.
const LISTING_MAX_BYTES: usize = 64 * 1024 * 1024;
/// git's stderr is only ever an error message; this much of it is kept.
const STDERR_MAX_BYTES: usize = 64 * 1024;

/// The empty tree in a SHA-1 repository — what everything is compared with before the first
/// commit. Asked of git first, which knows whether the repository is SHA-256 instead.
const EMPTY_TREE_SHA1: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Set with `-c` on every run. See the module docs.
const GIT_CONFIG: &[&str] = &["core.quotepath=false", "core.fsmonitor=false", "color.ui=never"];

const GIT_ENV: &[(&str, &str)] = &[
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("TERM", "dumb"),
    // English, so "not a git repository" can be recognised.
    ("LC_ALL", "C"),
    // A path is a path: `notes[1].md` is that file, not a glob matching `notes1.md`.
    ("GIT_LITERAL_PATHSPECS", "1"),
];

/// Inherited, these would point git at another repository, index or diff program than the one
/// in the folder — whatever started the Studio may have been a git hook.
const GIT_ENV_CLEARED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIFF_OPTS",
];

/// What every patch is made with.
const PATCH_FLAGS: &[&str] = &["--no-color", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/"];

// ---------------------------------------------------------------------------------------------
// The shapes the UI reads.
// ---------------------------------------------------------------------------------------------

/// What happened to a file, most specific first: a file both renamed and edited is `renamed`.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Conflicted,
    Untracked,
    Renamed,
    Copied,
    Deleted,
    Added,
    Typechange,
    Modified,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitFile {
    /// Relative to the workspace root, `/`-separated.
    pub path: String,
    /// The path before a rename or copy — with `../` when that was outside the root.
    pub orig_path: Option<String>,
    /// Porcelain v2's two letters: the index (staged) side and the worktree side; `.` is
    /// unchanged. `?` and `?` for an untracked file.
    pub index: String,
    pub worktree: String,
    pub status: FileStatus,
    /// Lines added and removed against HEAD; an untracked file's every line is added. `null` for
    /// a binary file, and for a new file too large (or too many) to count.
    pub added: Option<u64>,
    pub removed: Option<u64>,
    pub binary: bool,
}

/// `git status` of a workspace. A folder that is not in a repository is `is_git: false` with
/// everything else empty — an answer, not an error.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct GitStatus {
    pub is_git: bool,
    /// `null` on a detached HEAD.
    pub branch: Option<String>,
    /// HEAD's short commit id; `null` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub files: Vec<GitFile>,
    /// Sums over `files`, a `null` counting as 0.
    pub added: u64,
    pub removed: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitDiff {
    pub patch: String,
    /// The patch was cut at [`DIFF_MAX_BYTES`], or new files were left out past
    /// [`DIFF_MAX_NEW_FILES`].
    pub truncated: bool,
}

// ---------------------------------------------------------------------------------------------
// The two operations.
// ---------------------------------------------------------------------------------------------

/// **What changed in the workspace**: the branch and its upstream, and every changed, new or
/// conflicted file under the root with its line counts against HEAD.
pub async fn status(workspace: &Workspace) -> Result<GitStatus> {
    let root = workspace.root().to_path_buf();
    // A folder with no `.git` above it is answered without git, which need not even be installed.
    if !in_a_repository(&root).await {
        return Ok(GitStatus::default());
    }
    let git = Git::find(&root)?;
    let Some(listing) = git.listing().await? else { return Ok(GitStatus::default()) };
    let base = git.base(listing.unborn).await;
    let numstat = git.numstat(&base).await?;
    let new_files: Vec<String> = listing
        .entries
        .iter()
        .filter(|e| e.status == FileStatus::Untracked && !e.path.ends_with('/'))
        .map(|e| e.path.clone())
        .collect();
    let counted = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || count_new_files(&root, &new_files))
            .await
            .map_err(|e| Error::io("git status", std::io::Error::other(e.to_string())))?
    };

    let mut files: Vec<GitFile> = listing
        .entries
        .into_iter()
        .map(|entry| {
            let (added, removed, binary) = if entry.status == FileStatus::Untracked {
                match counted.get(&entry.path) {
                    Some(&(Some(lines), _)) => (Some(lines), Some(0), false),
                    Some(&(None, binary)) => (None, None, binary),
                    // A nested repository, listed as its folder.
                    None => (None, None, false),
                }
            } else {
                let renamed_from = (entry.status == FileStatus::Renamed).then_some(entry.orig_path.as_deref()).flatten();
                numstat.counts_for(&entry.path, renamed_from)
            };
            GitFile {
                path: entry.path,
                orig_path: entry.orig_path,
                index: entry.index.to_string(),
                worktree: entry.worktree.to_string(),
                status: entry.status,
                added,
                removed,
                binary,
            }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let added = files.iter().filter_map(|f| f.added).sum();
    let removed = files.iter().filter_map(|f| f.removed).sum();
    Ok(GitStatus {
        is_git: true,
        branch: listing.branch,
        head: listing.oid.map(|oid| oid.chars().take(7).collect()),
        upstream: listing.upstream,
        ahead: listing.ahead,
        behind: listing.behind,
        files,
        added,
        removed,
    })
}

/// **The unified diff of the working tree and index against HEAD** — the empty tree before the
/// first commit — for `path` (already checked by [`Workspace::resolve`]: root-relative, or `.`)
/// or the whole root when it is `None`.
///
/// New files are part of it: each is diffed against `/dev/null` (`git diff --no-index`), after
/// the tracked changes. A renamed file is diffed with its old name, so its patch is the rename and
/// the edit — not a new file beside a deleted one.
pub async fn diff(workspace: &Workspace, path: Option<&str>) -> Result<GitDiff> {
    let root = workspace.root().to_path_buf();
    let not_a_repository = || {
        bad(format!("{} is not in a git repository, so it has no diff — git status reports is_git: false for it", display_path(&root)))
    };
    if !in_a_repository(&root).await {
        return Err(not_a_repository());
    }
    let git = Git::find(&root)?;
    let Some(listing) = git.listing().await? else { return Err(not_a_repository()) };

    let target = path.map(|p| p.trim_end_matches('/')).filter(|p| !p.is_empty() && *p != ".");
    let covered = |p: &str| target.is_none_or(|t| p == t || p.strip_prefix(t).is_some_and(|rest| rest.starts_with('/')));
    let mut pathspecs = vec![target.unwrap_or(".").to_string()];
    let mut tracked = target.is_none();
    let mut new_files = Vec::new();
    for entry in listing.entries.iter().filter(|e| covered(&e.path)) {
        if entry.status == FileStatus::Untracked {
            // A nested repository is listed as its folder, which has no patch of its own.
            if !entry.path.ends_with('/') {
                new_files.push(entry.path.clone());
            }
            continue;
        }
        tracked = true;
        if let Some(orig) = &entry.orig_path
            && !covered(orig)
        {
            pathspecs.push(orig.clone());
        }
    }

    let mut patch = Vec::new();
    let mut truncated = false;
    if tracked {
        let base = git.base(listing.unborn).await;
        let mut args = vec!["diff", "--relative", "--find-renames", "--submodule=short"];
        args.extend_from_slice(PATCH_FLAGS);
        args.push(&base);
        args.push("--");
        args.extend(pathspecs.iter().map(String::as_str));
        let run = git.run(&args, DIFF_MAX_BYTES).await?;
        if !run.cut && !run.ok() {
            return Err(run.failure("diff"));
        }
        truncated = run.cut;
        patch = run.stdout;
    }
    new_files.sort();
    for (n, file) in new_files.iter().enumerate() {
        let room = DIFF_MAX_BYTES.saturating_sub(patch.len());
        if truncated || room == 0 || n >= DIFF_MAX_NEW_FILES {
            truncated = true;
            break;
        }
        let mut args = vec!["diff", "--no-index"];
        args.extend_from_slice(PATCH_FLAGS);
        args.extend(["--", "/dev/null", file.as_str()]);
        let run = git.run(&args, room).await?;
        // `--no-index` exits 1 when the two sides differ, which a new file's always do.
        if !run.cut && !matches!(run.code, Some(0 | 1)) {
            return Err(run.failure("diff --no-index"));
        }
        truncated = run.cut;
        patch.extend_from_slice(&run.stdout);
    }
    if truncated {
        cut_at_line(&mut patch);
    }
    Ok(GitDiff { patch: String::from_utf8_lossy(&patch).into_owned(), truncated })
}

async fn in_a_repository(root: &Path) -> bool {
    let root = root.to_path_buf();
    // A panic in the probe is no reason to skip asking git itself.
    tokio::task::spawn_blocking(move || find_git(&root).is_some()).await.unwrap_or(true)
}

/// A patch cut short ends after its last whole line.
fn cut_at_line(patch: &mut Vec<u8>) {
    let end = patch.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1);
    patch.truncate(end);
}

// ---------------------------------------------------------------------------------------------
// Finding and running git.
// ---------------------------------------------------------------------------------------------

/// **Which git to run**: the first on `PATH`, then the usual places — an app started from the
/// Dock or Finder has launchd's `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which has no Homebrew.
///
/// On macOS `/usr/bin/git` is not git but a shim for the developer tools' git, and when those are
/// not installed, running it opens a dialog offering to install them — every time. A UI that asks
/// for `git status` every few seconds must never be what raises it, so any other git is preferred
/// to the shim, and the shim is used only when the git it would run is there to be found.
fn git_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).collect())
        .unwrap_or_default();
    if cfg!(unix) {
        candidates.extend(["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"].map(PathBuf::from));
    }
    let shim = Path::new("/usr/bin/git");
    let is_shim = |path: &Path| cfg!(target_os = "macos") && path == shim;
    if let Some(found) = candidates.iter().find(|path| !is_shim(path) && is_executable(path)) {
        return Some(found.clone());
    }
    (cfg!(target_os = "macos") && is_executable(shim) && developer_tools_installed()).then(|| shim.to_path_buf())
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Whether the developer tools the macOS shim would run are installed — found by looking for
/// their git where the shim looks (`DEVELOPER_DIR`, the `xcode-select` choice, Xcode, the Command
/// Line Tools), not by running the shim to see what happens.
fn developer_tools_installed() -> bool {
    let chosen = std::env::var_os("DEVELOPER_DIR").map(PathBuf::from);
    chosen
        .into_iter()
        .chain(
            ["/var/db/xcode_select_link", "/Applications/Xcode.app/Contents/Developer", "/Library/Developer/CommandLineTools"]
                .map(PathBuf::from),
        )
        .any(|dir| dir.join("usr/bin/git").is_file())
}

fn not_installed() -> Error {
    let how = if cfg!(target_os = "macos") {
        "install Apple's Command Line Tools (`xcode-select --install`) or Homebrew's git (`brew install git`)"
    } else {
        "install git"
    };
    bad(format!(
        "this folder is a git repository, but git was not found (looked on PATH and in /opt/homebrew/bin, /usr/local/bin and \
         /usr/bin): {how}, then try again"
    ))
}

/// One git run's result.
struct Run {
    /// `None` when git was ended by a signal — or stopped here, past a cap.
    code: Option<i32>,
    stdout: Vec<u8>,
    /// stdout went past the most it was read for, and git was stopped there.
    cut: bool,
    stderr: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// git's own way of saying there is no work tree here: no repository at all, or a folder
    /// inside `.git` or a bare repository.
    fn not_a_repository(&self) -> bool {
        self.stderr.contains("not a git repository") || self.stderr.contains("must be run in a work tree")
    }

    fn failure(&self, what: &str) -> Error {
        let said = self.stderr.trim();
        let said = match (said.is_empty(), self.code) {
            (false, _) => said.to_string(),
            (true, Some(code)) => format!("exit status {code}"),
            (true, None) => "it was killed by a signal".to_string(),
        };
        bad(format!("git {what} failed: {said}"))
    }
}

/// git, pointed at a workspace root.
struct Git {
    binary: PathBuf,
    root: PathBuf,
}

impl Git {
    fn find(root: &Path) -> Result<Git> {
        Ok(Git { binary: git_binary().ok_or_else(not_installed)?, root: root.to_path_buf() })
    }

    /// **Run `git <args>` in the root**: no shell, stdin closed, its own process group (so a
    /// timeout stops what it started too), and stdout read up to `max_stdout` — past it git is
    /// stopped and the run is `cut`.
    async fn run(&self, args: &[&str], max_stdout: usize) -> Result<Run> {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("-C").arg(&self.root);
        for setting in GIT_CONFIG {
            cmd.arg("-c").arg(setting);
        }
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        for (key, value) in GIT_ENV {
            cmd.env(key, value);
        }
        for key in GIT_ENV_CLEARED {
            cmd.env_remove(key);
        }
        detach(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| bad(format!("could not run {}: {e}", display_path(&self.binary))))?;
        let mut group = ProcessGroup::new(child.id());
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let work = async {
            let stdout = async {
                let (bytes, cut) = read_capped(stdout, max_stdout, false).await;
                // Rather than leave git writing a patch nobody will read.
                if cut {
                    group.kill();
                }
                (bytes, cut)
            };
            let ((stdout, cut), (stderr, _)) = tokio::join!(stdout, read_capped(stderr, STDERR_MAX_BYTES, true));
            (stdout, cut, stderr, child.wait().await.ok())
        };
        // On a timeout `group` is dropped armed, which kills git and whatever it started.
        let Ok((stdout, cut, stderr, status)) = tokio::time::timeout(GIT_TIMEOUT, work).await else {
            let command = args.first().copied().unwrap_or_default();
            return Err(bad(format!(
                "git {command} did not finish within {} seconds in {} and was stopped",
                GIT_TIMEOUT.as_secs(),
                display_path(&self.root)
            )));
        };
        group.disarm();
        Ok(Run { code: status.and_then(|s| s.code()), stdout, cut, stderr: String::from_utf8_lossy(&stderr).into_owned() })
    }

    /// **`git status` of the root**, its paths made root-relative. `None` when the root is not in
    /// a work tree.
    async fn listing(&self) -> Result<Option<Porcelain>> {
        // Where the root is in the repository, and the status of that part of it — at once.
        let (probe, listing) = tokio::join!(
            self.run(&["rev-parse", "--is-inside-work-tree", "--show-prefix"], 64 * 1024),
            self.run(
                &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--renames", "--", "."],
                LISTING_MAX_BYTES
            ),
        );
        let probe = probe?;
        if !probe.ok() {
            return if probe.not_a_repository() { Ok(None) } else { Err(probe.failure("rev-parse")) };
        }
        let said = String::from_utf8_lossy(&probe.stdout);
        let mut lines = said.lines();
        // `false` inside `.git` itself or a bare repository: no working tree to report on.
        if lines.next() != Some("true") {
            return Ok(None);
        }
        let prefix = lines.next().unwrap_or_default().to_string();
        let listing = listing?;
        if !listing.ok() {
            return if listing.not_a_repository() { Ok(None) } else { Err(listing.failure("status")) };
        }
        if listing.cut {
            return Err(too_large("status"));
        }
        let mut porcelain = parse_porcelain(&listing.stdout);
        porcelain.entries = porcelain
            .entries
            .into_iter()
            .filter_map(|mut entry| {
                entry.path = entry.path.strip_prefix(&prefix).filter(|p| !p.is_empty())?.to_string();
                entry.orig_path = entry.orig_path.map(|orig| relative_to_root(&orig, &prefix));
                Some(entry)
            })
            .collect();
        Ok(Some(porcelain))
    }

    /// What the working tree is compared with: HEAD, or — before the first commit — the empty
    /// tree.
    async fn base(&self, unborn: bool) -> String {
        if !unborn {
            return "HEAD".to_string();
        }
        if let Ok(run) = self.run(&["hash-object", "-t", "tree", "--stdin"], 1024).await
            && run.ok()
        {
            let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return id;
            }
        }
        EMPTY_TREE_SHA1.to_string()
    }

    /// Lines added and removed per file under the root, working tree and index against `base`.
    async fn numstat(&self, base: &str) -> Result<Numstat> {
        let mut args = vec!["diff", "--numstat", "-z", "--relative", "--find-renames"];
        args.extend_from_slice(PATCH_FLAGS);
        args.extend([base, "--", "."]);
        let run = self.run(&args, LISTING_MAX_BYTES).await?;
        if run.cut {
            return Err(too_large("diff --numstat"));
        }
        if !run.ok() {
            return Err(run.failure("diff --numstat"));
        }
        Ok(parse_numstat(&run.stdout))
    }
}

fn too_large(what: &str) -> Error {
    bad(format!(
        "git {what} printed more than {} MiB — too many changed files to list; is a build or dependency folder missing from .gitignore?",
        LISTING_MAX_BYTES / (1024 * 1024)
    ))
}

/// Read a pipe to its end, keeping at most `max` bytes. Past that it is `cut`: with `drain` the
/// rest is read and dropped (git must not block on a full stderr); without, reading stops.
async fn read_capped<R: AsyncRead + Unpin>(stream: Option<R>, max: usize, drain: bool) -> (Vec<u8>, bool) {
    let Some(mut stream) = stream else { return (Vec::new(), false) };
    let mut kept = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut cut = false;
    loop {
        let n = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let room = max.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..n.min(room)]);
        if n > room {
            cut = true;
            if !drain {
                break;
            }
        }
    }
    (kept, cut)
}

/// A repository-relative path as the root names it. Inside the root (`prefix` is the root's own
/// place in the repository, `sub/` or empty): without the prefix. Outside it — the old name of a
/// file moved in — climbing out with `..`.
fn relative_to_root(path: &str, prefix: &str) -> String {
    let base: Vec<&str> = prefix.split('/').filter(|part| !part.is_empty()).collect();
    let parts: Vec<&str> = path.split('/').collect();
    let common = base.iter().zip(&parts).take_while(|(a, b)| a == b).count();
    let mut out = vec![".."; base.len() - common];
    out.extend(&parts[common..]);
    out.join("/")
}

// ---------------------------------------------------------------------------------------------
// Reading what git printed.
// ---------------------------------------------------------------------------------------------

/// What `git status --porcelain=v2 --branch -z` said.
#[derive(Debug, Default, PartialEq, Eq)]
struct Porcelain {
    /// HEAD's full commit id; `None` before the first commit.
    oid: Option<String>,
    /// `# branch.oid (initial)`: there is no commit yet.
    unborn: bool,
    /// `None` on a detached HEAD.
    branch: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    entries: Vec<Entry>,
}

/// One changed path. Repository-relative as git prints it; root-relative once
/// [`Git::listing`] has made it so.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    orig_path: Option<String>,
    index: char,
    worktree: char,
    status: FileStatus,
}

/// **Porcelain v2, NUL-separated.** Headers are `# key value`. Records: `1 XY sub mH mI mW hH hI
/// path` (changed), `2 XY sub mH mI mW hH hI Xscore path` followed by the old path as its own
/// field (renamed or copied), `u XY sub m1 m2 m3 mW h1 h2 h3 path` (unmerged), `? path`
/// (untracked) and `! path` (ignored, skipped). The path is everything after the fixed fields, so
/// a name with spaces needs no unquoting; a record git adds in some later version is skipped.
fn parse_porcelain(output: &[u8]) -> Porcelain {
    let mut out = Porcelain::default();
    let mut fields = output.split(|b| *b == 0).map(String::from_utf8_lossy);
    while let Some(record) = fields.next() {
        if let Some(header) = record.strip_prefix("# ") {
            out.header(header);
            continue;
        }
        let entry = match record.as_bytes().first() {
            Some(b'1') => changed(&record, 9, None),
            Some(b'2') => {
                let orig = fields.next().map(|f| f.into_owned());
                changed(&record, 10, orig)
            }
            Some(b'u') => changed(&record, 11, None).map(|entry| Entry { status: FileStatus::Conflicted, ..entry }),
            Some(b'?') => record.get(2..).filter(|p| !p.is_empty()).map(|path| Entry {
                path: path.to_string(),
                orig_path: None,
                index: '?',
                worktree: '?',
                status: FileStatus::Untracked,
            }),
            _ => None,
        };
        out.entries.extend(entry);
    }
    out
}

/// A `1`, `2` or `u` record: `fields` space-separated fields, the last of them the path.
fn changed(record: &str, fields: usize, orig_path: Option<String>) -> Option<Entry> {
    let parts: Vec<&str> = record.splitn(fields, ' ').collect();
    let path = parts.get(fields - 1).filter(|p| !p.is_empty())?;
    let mut xy = parts.get(1)?.chars();
    let (index, worktree) = (xy.next()?, xy.next()?);
    Some(Entry { path: path.to_string(), orig_path, index, worktree, status: classify(index, worktree) })
}

/// The most specific of what the two letters say: renamed, then copied, deleted, added, a type
/// change, and modified.
fn classify(index: char, worktree: char) -> FileStatus {
    let either = |letter: char| index == letter || worktree == letter;
    if either('U') {
        FileStatus::Conflicted
    } else if either('R') {
        FileStatus::Renamed
    } else if either('C') {
        FileStatus::Copied
    } else if either('D') {
        FileStatus::Deleted
    } else if either('A') {
        FileStatus::Added
    } else if either('T') {
        FileStatus::Typechange
    } else {
        FileStatus::Modified
    }
}

impl Porcelain {
    fn header(&mut self, line: &str) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "branch.oid" if value == "(initial)" => self.unborn = true,
            "branch.oid" => self.oid = Some(value.to_string()).filter(|oid| !oid.is_empty()),
            "branch.head" => self.branch = Some(value.to_string()).filter(|branch| branch != "(detached)" && !branch.is_empty()),
            "branch.upstream" => self.upstream = Some(value.to_string()).filter(|upstream| !upstream.is_empty()),
            "branch.ab" => {
                for count in value.split(' ') {
                    if let Some(ahead) = count.strip_prefix('+') {
                        self.ahead = ahead.parse().unwrap_or(0);
                    } else if let Some(behind) = count.strip_prefix('-') {
                        self.behind = behind.parse().unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }
}

/// What `git diff --numstat -z` said: lines added and removed per path, `None` for a binary file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Numstat {
    /// By the path after the change — a rename's new name.
    counts: HashMap<String, (Option<u64>, Option<u64>)>,
    /// The renames the diff found, as (old name, new name).
    renames: HashSet<(String, String)>,
}

/// `added\tremoved\tpath\0`, or for a rename or copy `added\tremoved\t\0old\0new\0`. A binary file
/// is `-\t-\t…`.
fn parse_numstat(output: &[u8]) -> Numstat {
    let mut out = Numstat::default();
    let mut fields = output.split(|b| *b == 0).map(String::from_utf8_lossy);
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let counts = (added.parse().ok(), removed.parse().ok());
        if path.is_empty() {
            let (Some(old), Some(new)) = (fields.next(), fields.next()) else { break };
            out.counts.insert(new.to_string(), counts);
            out.renames.insert((old.into_owned(), new.into_owned()));
        } else {
            out.counts.insert(path.to_string(), counts);
        }
    }
    out
}

impl Numstat {
    /// `(added, removed, binary)` for a file status listed. Status and diff find renames each on
    /// their own: when status saw `renamed_from → path` and the diff did not (the new file was
    /// edited past recognising), the diff has the old file's removal on its own line, and it is
    /// added in. A file the diff does not list has no line changed against HEAD.
    fn counts_for(&self, path: &str, renamed_from: Option<&str>) -> (Option<u64>, Option<u64>, bool) {
        let Some(&(added, removed)) = self.counts.get(path) else { return (Some(0), Some(0), false) };
        let (Some(mut added), Some(mut removed)) = (added, removed) else { return (None, None, true) };
        if let Some(old) = renamed_from
            && !self.renames.contains(&(old.to_string(), path.to_string()))
            && let Some(&(Some(old_added), Some(old_removed))) = self.counts.get(old)
        {
            added += old_added;
            removed += old_removed;
        }
        (Some(added), Some(removed), false)
    }
}

// ---------------------------------------------------------------------------------------------
// Counting the lines of new files.
// ---------------------------------------------------------------------------------------------

/// **Each new file's line count and whether it is binary**, as `git diff --numstat` would count
/// it as an addition — `None` for a binary file, one over [`COUNT_MAX_FILE_BYTES`], or one past
/// the budget.
fn count_new_files(root: &Path, paths: &[String]) -> HashMap<String, (Option<u64>, bool)> {
    let mut budget = COUNT_BUDGET_BYTES;
    paths
        .iter()
        .enumerate()
        .map(|(n, path)| {
            let counted = if n < COUNT_BUDGET_FILES { count_lines(&root.join(path), &mut budget) } else { (None, false) };
            (path.clone(), counted)
        })
        .collect()
}

fn count_lines(path: &Path, budget: &mut u64) -> (Option<u64>, bool) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return (None, false) };
    // git stores a link as the path it points to: one line.
    if meta.file_type().is_symlink() {
        return (Some(1), false);
    }
    if !meta.is_file() {
        return (None, false);
    }
    if meta.len() > COUNT_MAX_FILE_BYTES || meta.len() > *budget {
        // Too much to read for a count, but whether it is binary is still worth saying.
        return (None, sniff(path).is_ok_and(|head| looks_binary(&head)));
    }
    let Ok(bytes) = std::fs::read(path) else { return (None, false) };
    *budget = budget.saturating_sub(bytes.len() as u64);
    if looks_binary(&bytes) {
        return (None, true);
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    // A last line with no newline after it is still a line.
    let unterminated = bytes.last().is_some_and(|b| *b != b'\n');
    (Some(newlines + u64::from(unterminated)), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(fields: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for field in fields {
            out.extend_from_slice(field.as_bytes());
            out.push(0);
        }
        out
    }

    const OID: &str = "26087ff96232df2eb51aefe0fb4e63d95c14a135";
    const BLOB: &str = "975fbec8256d3e8a3797e7a3611380f27c49f4ac";

    #[test]
    fn porcelain_v2_records_of_every_kind() {
        let output = nul(&[
            &format!("# branch.oid {OID}"),
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +3 -1",
            &format!("2 R. N... 100644 100644 100644 {BLOB} {BLOB} R100 sub/renamed space.txt"),
            "sub/with space.txt",
            &format!("1 .M N... 100644 100644 100644 {BLOB} {BLOB} top.txt"),
            &format!("1 MD N... 100644 100644 000000 {BLOB} {BLOB} gone.txt"),
            &format!("1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 {BLOB} new.txt"),
            &format!("1 .T N... 120000 120000 100644 {BLOB} {BLOB} link"),
            &format!("2 C. N... 100644 100644 100644 {BLOB} {BLOB} C75 copy.txt"),
            "orig.txt",
            &format!("u UU N... 100644 100644 100644 100644 {BLOB} {BLOB} {BLOB} conflicted file.txt"),
            "? sub/deep/untracked file.txt",
            "? nested/",
            "! ignored.log",
            "z future record kind",
        ]);
        let parsed = parse_porcelain(&output);
        assert_eq!(parsed.oid.as_deref(), Some(OID));
        assert!(!parsed.unborn);
        assert_eq!(parsed.branch.as_deref(), Some("main"));
        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!((parsed.ahead, parsed.behind), (3, 1));
        let summary: Vec<(&str, Option<&str>, char, char, FileStatus)> =
            parsed.entries.iter().map(|e| (e.path.as_str(), e.orig_path.as_deref(), e.index, e.worktree, e.status)).collect();
        assert_eq!(
            summary,
            vec![
                ("sub/renamed space.txt", Some("sub/with space.txt"), 'R', '.', FileStatus::Renamed),
                ("top.txt", None, '.', 'M', FileStatus::Modified),
                ("gone.txt", None, 'M', 'D', FileStatus::Deleted),
                ("new.txt", None, 'A', '.', FileStatus::Added),
                ("link", None, '.', 'T', FileStatus::Typechange),
                ("copy.txt", Some("orig.txt"), 'C', '.', FileStatus::Copied),
                ("conflicted file.txt", None, 'U', 'U', FileStatus::Conflicted),
                ("sub/deep/untracked file.txt", None, '?', '?', FileStatus::Untracked),
                ("nested/", None, '?', '?', FileStatus::Untracked),
            ]
        );
    }

    #[test]
    fn porcelain_v2_headers_before_the_first_commit_and_when_detached() {
        let unborn = parse_porcelain(&nul(&["# branch.oid (initial)", "# branch.head trunk", "? a.txt"]));
        assert!(unborn.unborn);
        assert_eq!((unborn.oid, unborn.branch.as_deref()), (None, Some("trunk")));
        assert_eq!((unborn.upstream, unborn.ahead, unborn.behind), (None, 0, 0));
        assert_eq!(unborn.entries.len(), 1);

        let detached = parse_porcelain(&nul(&[&format!("# branch.oid {OID}"), "# branch.head (detached)"]));
        assert_eq!((detached.oid.as_deref(), detached.branch), (Some(OID), None));
        assert!(!detached.unborn && detached.entries.is_empty());
        assert_eq!(parse_porcelain(b""), Porcelain::default());
    }

    #[test]
    fn a_conflict_outranks_whatever_its_letters_say() {
        // `u` records carry AA, DD, AU, UD… — every one of them is a conflict.
        let output = nul(&[&format!("u AA N... 000000 100644 100644 100644 {BLOB} {BLOB} {BLOB} both added.txt")]);
        assert_eq!(parse_porcelain(&output).entries[0].status, FileStatus::Conflicted);
        assert_eq!(classify('R', 'M'), FileStatus::Renamed);
        assert_eq!(classify('A', 'D'), FileStatus::Deleted);
        assert_eq!(classify('A', 'M'), FileStatus::Added);
        assert_eq!(classify('M', 'T'), FileStatus::Typechange);
        assert_eq!(classify('M', 'M'), FileStatus::Modified);
    }

    #[test]
    fn numstat_lines_binaries_and_renames() {
        let output = nul(&["2\t1\ttop.txt", "-\t-\tbin.dat", "0\t0\t", "with space.txt", "renamed space.txt", "1\t1\ts.txt"]);
        let parsed = parse_numstat(&output);
        assert_eq!(parsed.counts.get("top.txt"), Some(&(Some(2), Some(1))));
        assert_eq!(parsed.counts.get("bin.dat"), Some(&(None, None)));
        assert_eq!(parsed.counts.get("renamed space.txt"), Some(&(Some(0), Some(0))));
        assert_eq!(parsed.counts.get("s.txt"), Some(&(Some(1), Some(1))));
        assert!(parsed.renames.contains(&("with space.txt".to_string(), "renamed space.txt".to_string())));
        assert_eq!(parsed.counts.len(), 4);

        assert_eq!(parsed.counts_for("top.txt", None), (Some(2), Some(1), false));
        assert_eq!(parsed.counts_for("bin.dat", None), (None, None, true));
        assert_eq!(parsed.counts_for("renamed space.txt", Some("with space.txt")), (Some(0), Some(0), false));
        assert_eq!(parsed.counts_for("unlisted.txt", None), (Some(0), Some(0), false), "nothing differs from HEAD");
    }

    #[test]
    fn a_rename_the_diff_did_not_see_counts_both_sides() {
        // Status paired old → new; the diff, comparing with the edited file, saw a deletion and an
        // addition instead.
        let parsed = parse_numstat(&nul(&["0\t5\told.txt", "7\t0\tnew.txt"]));
        assert_eq!(parsed.counts_for("new.txt", Some("old.txt")), (Some(7), Some(5), false));
    }

    #[test]
    fn paths_are_made_relative_to_the_root() {
        assert_eq!(relative_to_root("a/b.txt", ""), "a/b.txt");
        assert_eq!(relative_to_root("sub/deep/x.txt", "sub/"), "deep/x.txt");
        assert_eq!(relative_to_root("other/x.txt", "sub/deep/"), "../../other/x.txt");
        assert_eq!(relative_to_root("sub/x.txt", "sub/deep/"), "../x.txt");
        assert_eq!(relative_to_root("top.txt", "sub/"), "../top.txt");
    }

    #[test]
    fn new_files_are_counted_like_git_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let put = |name: &str, bytes: &[u8]| std::fs::write(dir.path().join(name), bytes).unwrap();
        put("lines.txt", b"a\nb\nc\n");
        put("no-newline.txt", b"a\nb");
        put("empty.txt", b"");
        put("binary.dat", b"PK\x03\x04\0\0\x01");
        let mut big = vec![b'x'; COUNT_MAX_FILE_BYTES as usize];
        big.push(b'\n');
        put("big.txt", &big);
        let mut big_binary = vec![0u8; 16];
        big_binary.resize(COUNT_MAX_FILE_BYTES as usize + 1, b'x');
        put("big.bin", &big_binary);
        let names = ["lines.txt", "no-newline.txt", "empty.txt", "binary.dat", "big.txt", "big.bin", "missing.txt"];
        let counted = count_new_files(dir.path(), &names.map(String::from));
        assert_eq!(counted["lines.txt"], (Some(3), false));
        assert_eq!(counted["no-newline.txt"], (Some(2), false));
        assert_eq!(counted["empty.txt"], (Some(0), false));
        assert_eq!(counted["binary.dat"], (None, true));
        assert_eq!(counted["big.txt"], (None, false), "too large to count, and not binary");
        assert_eq!(counted["big.bin"], (None, true), "too large to count, and binary");
        assert_eq!(counted["missing.txt"], (None, false));
    }

    #[test]
    fn a_cut_patch_ends_at_a_whole_line() {
        let mut patch = b"diff --git a/x b/x\n+one\n+tw".to_vec();
        cut_at_line(&mut patch);
        assert_eq!(patch, b"diff --git a/x b/x\n+one\n");
        let mut one_long_line = b"+no newline at all".to_vec();
        cut_at_line(&mut one_long_line);
        assert!(one_long_line.is_empty());
    }
}
