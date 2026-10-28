//! **The Code side's git and terminal endpoints, through the Studio's own HTTP router.**
//!
//! `git/status` and `git/diff` against real repositories made here with the real `git` (skipped
//! when there is none): a workspace at the top of one and in a subdirectory of it, a plain
//! folder, a repository with no commit yet, a detached HEAD, an upstream. `exec/stream` as the
//! terminal uses it: output as it is printed, the two pipes apart, the exit, the timeout — and a
//! dropped connection killing what the command started. And the guard in front of all three.

use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

struct Studio {
    url: String,
    http: reqwest::Client,
    _data: tempfile::TempDir,
}

async fn studio() -> Studio {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Mock, ..Default::default() },
        ..Default::default()
    };
    let state = misaka_studio_runtime::AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state, None, Vec::new());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Studio { url, http: reqwest::Client::new(), _data: data }
}

impl Studio {
    async fn post(&self, route: &str, body: Value) -> (u16, Value) {
        self.post_with(route, body, &[]).await
    }

    async fn post_with(&self, route: &str, body: Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let response = self.request(route, body, headers).await;
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn request(&self, route: &str, body: Value, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = self.http.post(format!("{}/api/v1/workspace/{route}", self.url)).json(&body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    async fn stream(&self, body: Value) -> Events {
        let started = Instant::now();
        let response = self.request("exec/stream", body, &[]).await;
        assert_eq!(response.status().as_u16(), 200);
        let content_type = response.headers()["content-type"].to_str().unwrap().to_string();
        assert!(content_type.starts_with("text/event-stream"), "{content_type}");
        Events { response, buffer: Vec::new(), pending: VecDeque::new(), started }
    }
}

/// A server-sent-event stream, read the way the UI reads it: each `data:` line one JSON event,
/// with when it arrived.
struct Events {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: VecDeque<(Value, Duration)>,
    started: Instant,
}

impl Events {
    async fn next(&mut self) -> Option<(Value, Duration)> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            let chunk = self.response.chunk().await.unwrap()?;
            let arrived = self.started.elapsed();
            self.buffer.extend_from_slice(&chunk);
            while let Some(end) = self.buffer.windows(2).position(|w| w == b"\n\n") {
                let frame: Vec<u8> = self.buffer.drain(..end + 2).collect();
                for line in String::from_utf8(frame).unwrap().lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        let event = serde_json::from_str(data.trim()).unwrap_or_else(|e| panic!("{data}: {e}"));
                        self.pending.push_back((event, arrived));
                    }
                }
            }
        }
    }

    async fn all(mut self) -> Vec<Value> {
        let mut events = Vec::new();
        while let Some((event, _)) = self.next().await {
            events.push(event);
        }
        events
    }
}

fn joined(events: &[Value], kind: &str) -> String {
    events.iter().filter(|e| e["type"] == kind).map(|e| e["data"].as_str().unwrap()).collect()
}

// ---------------------------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------------------------

fn have_git() -> bool {
    let found = Command::new("git").arg("--version").output().is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("git is not installed; skipping");
    }
    found
}

/// Run git for the test's own setup, untouched by the person's global config (a signing key or
/// a hook there must not decide whether a test commit works).
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn put(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn init(dir: &Path) {
    git(dir, &["init", "-q"]);
    // `init -b` is newer than some gits still in use.
    git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
}

/// A repository with one commit, then: an edit, a deletion, a binary change, a staged rename
/// (names with spaces), an edit in a subfolder, and two new files. `same.txt` stays as it was.
fn changed_repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    init(dir);
    put(dir, "a.txt", "one\ntwo\nthree\n");
    put(dir, "gone.txt", "bye\n");
    put(dir, "bin.dat", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
    put(dir, "sub/s.txt", "s\n");
    put(dir, "sub/old name.txt", "keep\nthese\nlines\n");
    put(dir, "same.txt", "unchanged\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);

    put(dir, "a.txt", "one\nTWO\nthree\nfour\n");
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    put(dir, "bin.dat", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0changed");
    put(dir, "sub/s.txt", "s\ns2\n");
    git(dir, &["mv", "sub/old name.txt", "sub/new name.txt"]);
    put(dir, "new file.txt", "n1\nn2\n");
    put(dir, "sub/deep/new.txt", "d\n");
    repo
}

/// `(path, status, index, worktree, added, removed, binary, orig_path)` of one file.
type Row = (String, String, String, String, Value, Value, bool, Value);

fn summary(status: &Value) -> Vec<Row> {
    status["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["status"].as_str().unwrap().to_string(),
                f["index"].as_str().unwrap().to_string(),
                f["worktree"].as_str().unwrap().to_string(),
                f["added"].clone(),
                f["removed"].clone(),
                f["binary"].as_bool().unwrap(),
                f["orig_path"].clone(),
            )
        })
        .collect()
}

fn row(path: &str, status: &str, xy: &str, added: Value, removed: Value, binary: bool, orig: Value) -> Row {
    let (x, y) = xy.split_at(1);
    (path.into(), status.into(), x.into(), y.into(), added, removed, binary, orig)
}

#[tokio::test(flavor = "multi_thread")]
async fn git_status_lists_every_kind_of_change_with_line_counts() {
    if !have_git() {
        return;
    }
    let studio = studio().await;
    let repo = changed_repository();
    let root = repo.path().to_str().unwrap();

    let (status, body) = studio.post("git/status", json!({ "root": root })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["is_git"], true);
    assert_eq!(body["branch"], "main");
    let head = body["head"].as_str().unwrap();
    assert_eq!(head.len(), 7);
    assert!(git(repo.path(), &["rev-parse", "HEAD"]).starts_with(head));
    assert_eq!((body["upstream"].clone(), body["ahead"].clone(), body["behind"].clone()), (Value::Null, json!(0), json!(0)));
    assert_eq!(
        summary(&body),
        vec![
            row("a.txt", "modified", ".M", json!(2), json!(1), false, Value::Null),
            row("bin.dat", "modified", ".M", Value::Null, Value::Null, true, Value::Null),
            row("gone.txt", "deleted", ".D", json!(0), json!(1), false, Value::Null),
            row("new file.txt", "untracked", "??", json!(2), json!(0), false, Value::Null),
            row("sub/deep/new.txt", "untracked", "??", json!(1), json!(0), false, Value::Null),
            row("sub/new name.txt", "renamed", "R.", json!(0), json!(0), false, json!("sub/old name.txt")),
            row("sub/s.txt", "modified", ".M", json!(1), json!(0), false, Value::Null),
        ],
        "{body:#}"
    );
    assert_eq!((body["added"].clone(), body["removed"].clone()), (json!(6), json!(2)));
    // Status reads; it never takes the lock the person's own `git commit` needs.
    assert!(!repo.path().join(".git/index.lock").exists());

    // A workspace that is a subfolder of the repository sees its own files, named from there.
    let sub = repo.path().join("sub");
    let (status, body) = studio.post("git/status", json!({ "root": sub.to_str().unwrap() })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        summary(&body),
        vec![
            row("deep/new.txt", "untracked", "??", json!(1), json!(0), false, Value::Null),
            row("new name.txt", "renamed", "R.", json!(0), json!(0), false, json!("old name.txt")),
            row("s.txt", "modified", ".M", json!(1), json!(0), false, Value::Null),
        ],
        "{body:#}"
    );
    assert_eq!((body["added"].clone(), body["removed"].clone()), (json!(2), json!(0)));
    assert_eq!(body["branch"], "main");

    // Detached: no branch, the same commit.
    git(repo.path(), &["checkout", "-q", "--detach"]);
    let (_, body) = studio.post("git/status", json!({ "root": root })).await;
    assert_eq!((body["branch"].clone(), body["head"].as_str()), (Value::Null, Some(head)));
}

#[tokio::test(flavor = "multi_thread")]
async fn git_diff_of_the_root_a_file_a_rename_and_a_new_file() {
    if !have_git() {
        return;
    }
    let studio = studio().await;
    let repo = changed_repository();
    let root = repo.path().to_str().unwrap();

    let (status, whole) = studio.post("git/diff", json!({ "root": root })).await;
    assert_eq!(status, 200, "{whole}");
    assert_eq!(whole["truncated"], false);
    let patch = whole["patch"].as_str().unwrap();
    for expected in [
        "diff --git a/a.txt b/a.txt",
        "-two\n+TWO\n three\n+four\n",
        "deleted file mode 100644",
        "-bye\n",
        "Binary files a/bin.dat and b/bin.dat differ",
        "rename from sub/old name.txt\nrename to sub/new name.txt",
        "+++ b/new file.txt",
        "+n1\n+n2\n",
        "+++ b/sub/deep/new.txt",
    ] {
        assert!(patch.contains(expected), "{expected:?} missing from:\n{patch}");
    }
    assert!(!patch.contains("\x1b["), "no colour");

    // One file: its own patch and nothing else.
    let (_, one) = studio.post("git/diff", json!({ "root": root, "path": "a.txt" })).await;
    let one = one["patch"].as_str().unwrap();
    assert!(one.starts_with("diff --git a/a.txt b/a.txt\n"), "{one}");
    assert_eq!(one.matches("diff --git").count(), 1, "{one}");

    // A new file is diffed against nothing; a renamed one with its old name.
    let (status, new) = studio.post("git/diff", json!({ "root": root, "path": "new file.txt" })).await;
    assert_eq!(status, 200, "{new}");
    assert!(new["patch"].as_str().unwrap().contains("new file mode"), "{new}");
    assert!(new["patch"].as_str().unwrap().contains("+n1\n+n2\n"), "{new}");
    let (_, renamed) = studio.post("git/diff", json!({ "root": root, "path": "sub/new name.txt" })).await;
    assert!(renamed["patch"].as_str().unwrap().contains("rename from sub/old name.txt"), "{renamed}");
    // A folder: everything under it. An unchanged file: nothing. A null path: the whole root.
    let (_, folder) = studio.post("git/diff", json!({ "root": root, "path": "sub" })).await;
    assert_eq!(folder["patch"].as_str().unwrap().matches("diff --git").count(), 3, "{folder}");
    let (status, unchanged) = studio.post("git/diff", json!({ "root": root, "path": "same.txt" })).await;
    assert_eq!((status, unchanged), (200, json!({ "patch": "", "truncated": false })));
    let (_, null_path) = studio.post("git/diff", json!({ "root": root, "path": null })).await;
    assert_eq!(null_path["patch"], whole["patch"]);

    // From a subfolder workspace, names are the subfolder's — and its parent is out of reach.
    let sub = repo.path().join("sub");
    let sub = sub.to_str().unwrap();
    let (_, from_sub) = studio.post("git/diff", json!({ "root": sub, "path": "new name.txt" })).await;
    assert!(from_sub["patch"].as_str().unwrap().contains("rename from old name.txt\nrename to new name.txt"), "{from_sub}");
    let (_, whole_sub) = studio.post("git/diff", json!({ "root": sub })).await;
    let whole_sub = whole_sub["patch"].as_str().unwrap();
    assert!(whole_sub.contains("+++ b/s.txt") && whole_sub.contains("+++ b/deep/new.txt"), "{whole_sub}");
    assert!(!whole_sub.contains("a.txt"), "{whole_sub}");
    let (status, escaped) = studio.post("git/diff", json!({ "root": sub, "path": "../a.txt" })).await;
    assert_eq!(status, 400);
    assert!(escaped["error"]["message"].as_str().unwrap().starts_with("path escapes the workspace"), "{escaped}");

    // Past 1 MiB the patch is cut at a line boundary.
    let lines: String = (0..200_000).map(|n| format!("line {n}\n")).collect();
    put(repo.path(), "big.txt", lines);
    let (_, big) = studio.post("git/diff", json!({ "root": root, "path": "big.txt" })).await;
    assert_eq!(big["truncated"], true);
    let big = big["patch"].as_str().unwrap();
    assert!(big.len() <= 1024 * 1024 && big.len() > 1000 * 1024, "{}", big.len());
    assert!(big.ends_with('\n'));
    assert!(big.lines().last().unwrap().starts_with("+line "), "a whole line last");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_folder_is_not_git_and_has_no_diff() {
    let studio = studio().await;
    let plain = tempfile::tempdir().unwrap();
    put(plain.path(), "notes.txt", "x\n");
    // A `.git` that git does not accept as a repository is no repository either.
    put(plain.path(), "fake/.git/HEAD", "ref: refs/heads/main\n");
    for dir in [plain.path().to_path_buf(), plain.path().join("fake")] {
        let root = dir.to_str().unwrap();
        let (status, body) = studio.post("git/status", json!({ "root": root })).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body,
            json!({
                "is_git": false, "branch": null, "head": null, "upstream": null, "ahead": 0, "behind": 0,
                "files": [], "added": 0, "removed": 0
            })
        );
        if dir.join(".git").exists() && !have_git() {
            continue;
        }
        let (status, body) = studio.post("git/diff", json!({ "root": root })).await;
        assert_eq!(status, 400, "{body}");
        assert!(body["error"]["message"].as_str().unwrap().contains("is not in a git repository"), "{body}");
    }
    let (status, body) = studio.post("git/status", json!({ "root": "relative/path" })).await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn before_the_first_commit_and_with_an_upstream() {
    if !have_git() {
        return;
    }
    let studio = studio().await;
    let repo = tempfile::tempdir().unwrap();
    init(repo.path());
    put(repo.path(), "staged.txt", "a\nb\n");
    git(repo.path(), &["add", "staged.txt"]);
    put(repo.path(), "loose.txt", "c\n");
    let root = repo.path().to_str().unwrap();

    let (status, body) = studio.post("git/status", json!({ "root": root })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!((body["branch"].as_str(), body["head"].clone()), (Some("main"), Value::Null));
    assert_eq!(
        summary(&body),
        vec![
            row("loose.txt", "untracked", "??", json!(1), json!(0), false, Value::Null),
            row("staged.txt", "added", "A.", json!(2), json!(0), false, Value::Null),
        ]
    );
    let (status, diff) = studio.post("git/diff", json!({ "root": root })).await;
    assert_eq!(status, 200, "{diff}");
    let patch = diff["patch"].as_str().unwrap();
    assert!(patch.contains("+++ b/staged.txt") && patch.contains("+++ b/loose.txt"), "{patch}");

    // A clone, one commit ahead of where it came from.
    git(repo.path(), &["commit", "-q", "-m", "first"]);
    let clone = tempfile::tempdir().unwrap();
    git(clone.path(), &["clone", "-q", root, "."]);
    put(clone.path(), "staged.txt", "a\nb\nc\n");
    git(clone.path(), &["commit", "-q", "-am", "second"]);
    let (_, body) = studio.post("git/status", json!({ "root": clone.path().to_str().unwrap() })).await;
    assert_eq!(body["upstream"], "origin/main", "{body}");
    assert_eq!((body["ahead"].clone(), body["behind"].clone()), (json!(1), json!(0)));
    assert_eq!(body["files"], json!([]));
}

// ---------------------------------------------------------------------------------------------
// exec/stream
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn output_arrives_as_it_is_printed() {
    let studio = studio().await;
    let dir = tempfile::tempdir().unwrap();
    let mut events = studio.stream(json!({ "root": dir.path().to_str().unwrap(), "command": "printf a; sleep 1; printf b" })).await;
    let (first, first_at) = events.next().await.unwrap();
    assert_eq!(first, json!({ "type": "stdout", "data": "a" }));
    let (second, second_at) = events.next().await.unwrap();
    assert_eq!(second, json!({ "type": "stdout", "data": "b" }));
    assert!(second_at - first_at >= Duration::from_millis(800), "`a` came before `b` was printed: {first_at:?} {second_at:?}");
    let (exit, _) = events.next().await.unwrap();
    assert_eq!(exit["type"], "exit");
    assert_eq!((exit["exit_code"].clone(), exit["timed_out"].clone()), (json!(0), json!(false)));
    assert!(exit["duration_ms"].as_u64().unwrap() >= 1000, "{exit}");
    assert!(events.next().await.is_none(), "the exit is the last event");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_pipes_stay_apart_and_the_exit_code_comes_last() {
    let studio = studio().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let events = studio.stream(json!({ "root": root, "command": "echo out; echo err >&2; pwd; exit 3" })).await.all().await;
    let stdout = joined(&events, "stdout");
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("out"));
    assert_eq!(std::fs::canonicalize(lines.next().unwrap()).unwrap(), std::fs::canonicalize(dir.path()).unwrap());
    assert_eq!(joined(&events, "stderr"), "err\n");
    let exit = events.last().unwrap();
    assert_eq!(exit["type"], "exit", "{events:?}");
    assert_eq!(exit["exit_code"], 3);
    assert_eq!(exit["timed_out"], false);
    assert_eq!(exit.as_object().unwrap().len(), 4, "type, exit_code, timed_out, duration_ms: {exit}");

    // Refused before any stream starts: an ordinary error response.
    let (status, body) = studio.post("exec/stream", json!({ "root": root, "command": " " })).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["message"], "command is empty");
    let (status, _) = studio.post("exec/stream", json!({ "root": "/definitely/not/here", "command": "ls" })).await;
    assert_eq!(status, 400);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_command_times_out() {
    let studio = studio().await;
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let events = studio
        .stream(json!({ "root": dir.path().to_str().unwrap(), "command": "echo begun; sleep 30", "timeout_secs": "1" }))
        .await
        .all()
        .await;
    assert_eq!(joined(&events, "stdout"), "begun\n");
    let exit = events.last().unwrap();
    assert_eq!(
        (exit["type"].as_str(), exit["exit_code"].clone(), exit["timed_out"].clone()),
        (Some("exit"), Value::Null, json!(true))
    );
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

/// Alive, and not merely a zombie waiting for its parent to notice.
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    let out = Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_stream_kills_what_the_command_started() {
    let studio = studio().await;
    let dir = tempfile::tempdir().unwrap();
    let command = "sleep 30 & echo $! > sleep.pid; echo started; wait";
    let mut events = studio.stream(json!({ "root": dir.path().to_str().unwrap(), "command": command })).await;
    let (first, _) = events.next().await.unwrap();
    assert_eq!(first, json!({ "type": "stdout", "data": "started\n" }));
    let pid = std::fs::read_to_string(dir.path().join("sleep.pid")).unwrap().trim().to_string();
    assert!(alive(&pid), "the background sleep is running");

    // What the terminal's Ctrl+C does: the UI aborts the request.
    drop(events);
    let dropped = Instant::now();
    while alive(&pid) {
        assert!(dropped.elapsed() < Duration::from_secs(5), "sleep {pid} outlived the connection");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_page_reaches_none_of_them() {
    let studio = studio().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let evil = [("Origin", "https://evil.example")];
    for (route, body) in [
        ("git/status", json!({ "root": root })),
        ("git/diff", json!({ "root": root })),
        ("exec/stream", json!({ "root": root, "command": "touch pwned" })),
    ] {
        let (status, refused) = studio.post_with(route, body.clone(), &evil).await;
        assert_eq!(status, 403, "{route}: {refused}");
        assert_eq!(refused["error"]["code"], "forbidden", "{route}");
        let (status, _) = studio.post_with(route, body, &[("Host", "evil.example:1338")]).await;
        assert_eq!(status, 403, "{route}, rebound Host");
    }
    assert!(!dir.path().join("pwned").exists(), "a refused stream runs nothing");
    // The Studio's own window gets through.
    let (status, _) = studio.post_with("git/status", json!({ "root": root }), &[("Origin", "http://localhost:5174")]).await;
    assert_eq!(status, 200);
}
