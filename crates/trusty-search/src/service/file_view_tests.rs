//! Tests for `service::file_view` (#9029).
//!
//! What: every refusal arm, both caps, the happy paths with and without a
//! `HEAD` diff, and the git failure arms, against a real git repository in a
//! tempdir.
//! Test: `cargo test -p trusty-search -- file_view`.

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::IndexId;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A git repo with `src/lib.rs` committed, indexed as one bare handle.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    handle: IndexHandle,
}

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t.t", "-c", "user.name=t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A committed repo under a tempdir, as `(tempdir, canonical root)`.
pub(crate) fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join("src")).expect("src");
    let root = root.canonicalize().expect("canonical root");
    std::fs::write(root.join("src/lib.rs"), "pub fn one() {}\n").expect("lib.rs");
    std::fs::write(root.join("src/kept.rs"), "pub fn kept() {}\n").expect("kept.rs");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "init"]);
    (tmp, root)
}

fn fixture() -> Fixture {
    let (tmp, root) = repo();
    let indexer = Arc::new(RwLock::new(CodeIndexer::new("fv-9029", &root)));
    let handle = IndexHandle::bare(IndexId::new("fv-9029"), indexer, root.clone());
    Fixture {
        _tmp: tmp,
        root,
        handle,
    }
}

fn get(fx: &Fixture, path: &str, diff: DiffMode) -> Result<Value, Refusal> {
    read_indexed_file(&fx.handle, path, diff, &Limits::default())
}

/// #9029: `diff` defaults to none; content comes back whole, by relative or
/// absolute path, with `diff: null`.
#[test]
fn content_only_returns_the_file_and_no_diff() {
    let fx = fixture();
    let abs = fx.root.join("src/lib.rs");
    for path in ["src/lib.rs", abs.to_str().expect("utf-8 path")] {
        let body = get(&fx, path, DiffMode::None).expect("an indexed file is served");
        assert_eq!(body["path"], "src/lib.rs", "{body}");
        assert_eq!(body["content"], "pub fn one() {}\n");
        assert_eq!(body["content_truncated"], false);
        assert_eq!(body["size_bytes"], 16);
        assert_eq!(body["lossy_utf8"], false);
        assert!(body["diff"].is_null(), "{body}");
    }
}

/// #9029: `diff: head` reports a working-tree edit as a unified diff, a clean
/// file as `unchanged`, and a new file as `untracked`.
#[test]
fn head_diff_reports_a_working_tree_change() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("src/lib.rs"),
        "pub fn one() {}\npub fn two() {}\n",
    )
    .expect("edit");
    std::fs::write(fx.root.join("src/new.rs"), "pub fn new() {}\n").expect("new file");

    let changed = get(&fx, "src/lib.rs", DiffMode::Head).expect("served");
    assert_eq!(changed["content"], "pub fn one() {}\npub fn two() {}\n");
    let diff = &changed["diff"];
    assert_eq!(diff["status"], "changed", "{diff}");
    assert_eq!(diff["base"], "HEAD");
    assert_eq!(diff["truncated"], false);
    assert!(
        diff["text"]
            .as_str()
            .is_some_and(|t| t.contains("+pub fn two() {}")),
        "{diff}"
    );

    let clean = get(&fx, "src/kept.rs", DiffMode::Head).expect("served");
    assert_eq!(clean["diff"]["status"], "unchanged", "{clean}");
    assert_eq!(clean["diff"]["text"], "");

    let new = get(&fx, "src/new.rs", DiffMode::Head).expect("served");
    assert_eq!(new["diff"]["status"], "untracked", "{new}");
}

/// #9029: a missing file, a real file outside the root, and every file the
/// walker excludes — a skip dir, a gitignored file, an `exclude_globs` match,
/// `.env`, `id_rsa` — answer one byte-identical 404, so the method cannot
/// probe the filesystem.
#[test]
fn a_missing_file_and_an_outside_file_answer_one_body() {
    let mut fx = fixture();
    fx.handle.exclude_globs = vec!["**/by_glob.rs".to_owned()];
    let outside = fx._tmp.path().join("outside.rs");
    std::fs::write(&outside, "secret\n").expect("outside file");
    std::fs::create_dir_all(fx.root.join("node_modules/pkg")).expect("skip dir");
    std::fs::write(fx.root.join("node_modules/pkg/index.js"), "x\n").expect("excluded");
    std::fs::write(fx.root.join(".gitignore"), "src/ignored.rs\n").expect(".gitignore");
    std::fs::write(fx.root.join("src/ignored.rs"), "pub fn i() {}\n").expect("ignored");
    std::fs::write(fx.root.join("src/by_glob.rs"), "pub fn g() {}\n").expect("by glob");
    std::fs::write(fx.root.join(".env"), "API_TOKEN=secret\n").expect(".env");
    std::fs::write(
        fx.root.join("id_rsa"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
    )
    .expect("id_rsa");

    let missing = get(&fx, "src/nope.rs", DiffMode::None).expect_err("missing");
    assert_eq!(missing.0, StatusCode::NOT_FOUND);
    assert_eq!(missing.1["error"], "file_not_found");
    for path in [
        outside.to_str().expect("utf-8 path"),
        "node_modules/pkg/index.js",
        "src",
        "src/ignored.rs",
        "src/by_glob.rs",
        ".env",
        "id_rsa",
    ] {
        let refused = get(&fx, path, DiffMode::Head).expect_err(path);
        assert_eq!(
            refused, missing,
            "{path} must look exactly like a missing file"
        );
    }
}

/// #9029: a held index (#9059) answers one retryable 503 for every path, so
/// an existing excluded file and a missing one cannot be told apart.
#[test]
fn a_held_index_answers_one_refusal_for_every_path() {
    let mut fx = fixture();
    fx.handle.exclude_globs = vec!["**/secrets/[**".to_owned()];
    std::fs::write(fx.root.join(".env"), "API_TOKEN=secret\n").expect(".env");

    let existing = get(&fx, ".env", DiffMode::None).expect_err("held: existing");
    let missing = get(&fx, "src/nope.rs", DiffMode::None).expect_err("held: missing");
    assert_eq!(existing, missing, "a held index must not reveal existence");
    assert_eq!(
        existing.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        existing.1
    );
    assert_eq!(existing.1["reason"], "index_held");
    assert_eq!(existing.1["retryable"], true);
}

/// #9029: a `..` segment, an empty path and a NUL byte are refused as invalid
/// params before any filesystem access.
#[test]
fn traversal_is_refused_before_the_filesystem() {
    let fx = fixture();
    for (path, reason) in [
        ("../outside.rs", "path_traversal"),
        ("src/../../outside.rs", "path_traversal"),
        ("", "empty_or_nul"),
        ("src/lib.rs\0", "empty_or_nul"),
    ] {
        let (status, body) = get(&fx, path, DiffMode::None).expect_err(path);
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}: {body}");
        assert_eq!(body["error"], "invalid_path");
        assert_eq!(body["reason"], reason, "{path:?}");
    }
}

/// #9029: a symlink inside the root that resolves outside it is not served,
/// and answers the same 404 as a missing file.
#[cfg(unix)]
#[test]
fn a_symlink_escaping_the_root_is_not_found() {
    let fx = fixture();
    let outside = fx._tmp.path().join("outside.rs");
    std::fs::write(&outside, "secret\n").expect("outside file");
    std::os::unix::fs::symlink(&outside, fx.root.join("src/link.rs")).expect("symlink");

    let refused = get(&fx, "src/link.rs", DiffMode::None).expect_err("escaping link");
    let missing = get(&fx, "src/nope.rs", DiffMode::None).expect_err("missing");
    assert_eq!(refused, missing);
}

/// #9029: sops content is refused with 403, including when the metadata block
/// sits past the content cap.
#[test]
fn sops_content_is_refused() {
    let fx = fixture();
    let doc = format!(
        "password: {}\n{}sops:\n    version: 3.8.1\n",
        crate::core::sops::enc("aGk="),
        "padding: value\n".repeat(64)
    );
    std::fs::write(fx.root.join("config.yaml"), &doc).expect("sops file");
    for cap in [CONTENT_CAP_BYTES, 128] {
        let limits = Limits {
            content_cap: cap,
            ..Limits::default()
        };
        let (status, body) = read_indexed_file(&fx.handle, "config.yaml", DiffMode::None, &limits)
            .expect_err("a sops file is refused");
        assert_eq!(status, StatusCode::FORBIDDEN, "cap {cap}: {body}");
        assert_eq!(body["error"], "file_refused");
        assert_eq!(body["reason"], "sops_encrypted");
    }
}

/// #9029: content past its cap and a diff past its cap are cut and flagged,
/// never silently shortened; a character split by the cap is dropped whole.
#[test]
fn content_and_diff_over_their_caps_are_cut_and_flagged() {
    assert_eq!(CONTENT_CAP_BYTES, 1_048_576);
    assert_eq!(DIFF_CAP_BYTES, 262_144);
    let fx = fixture();
    std::fs::write(fx.root.join("src/lib.rs"), "// é\n".repeat(40)).expect("edit");
    let limits = Limits {
        content_cap: 4,
        diff_cap: 32,
        ..Limits::default()
    };
    let body =
        read_indexed_file(&fx.handle, "src/lib.rs", DiffMode::Head, &limits).expect("served");
    assert_eq!(body["content_truncated"], true, "{body}");
    assert_eq!(body["content"], "// ", "the split `é` is dropped: {body}");
    assert_eq!(body["lossy_utf8"], false);
    assert_eq!(body["content_cap_bytes"], 4);
    assert_eq!(body["size_bytes"], 240);
    let diff = &body["diff"];
    assert_eq!(diff["status"], "changed", "{diff}");
    assert_eq!(diff["truncated"], true, "{diff}");
    assert_eq!(diff["cap_bytes"], 32);
    assert!(
        diff["text"].as_str().is_some_and(|t| t.len() <= 32),
        "{diff}"
    );
}

/// #9029 fail-closed: when git cannot produce the requested diff, the content
/// still comes back and `diff.status` is `error` — never `unchanged`, never
/// an empty diff. Covers a non-repository, a missing binary, and a hang.
#[test]
fn a_git_failure_is_a_diff_error_not_an_empty_diff() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("plain");
    std::fs::create_dir_all(&root).expect("root");
    let root = root.canonicalize().expect("canonical");
    std::fs::write(root.join("lib.rs"), "pub fn x() {}\n").expect("file");
    let indexer = Arc::new(RwLock::new(CodeIndexer::new("fv-plain", &root)));
    let plain = IndexHandle::bare(IndexId::new("fv-plain"), indexer, root.clone());

    let not_a_repo = read_indexed_file(&plain, "lib.rs", DiffMode::Head, &Limits::default())
        .expect("content is still served");
    assert_eq!(not_a_repo["content"], "pub fn x() {}\n");
    assert_eq!(not_a_repo["diff"]["status"], "error", "{not_a_repo}");
    assert_eq!(not_a_repo["diff"]["error"], "git_failed");

    let fx = fixture();
    let missing_bin = Limits {
        git_bin: "/nonexistent/git-9029".to_owned(),
        ..Limits::default()
    };
    let body = read_indexed_file(&fx.handle, "src/lib.rs", DiffMode::Head, &missing_bin)
        .expect("content is still served");
    assert_eq!(body["diff"]["error"], "git_unavailable", "{body}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hang = tmp.path().join("hang-git.sh");
        std::fs::write(&hang, "#!/bin/sh\nsleep 30\n").expect("fake git");
        std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let hung = Limits {
            git_bin: hang.to_string_lossy().into_owned(),
            git_timeout: Duration::from_millis(200),
            ..Limits::default()
        };
        let started = std::time::Instant::now();
        let body = read_indexed_file(&fx.handle, "src/lib.rs", DiffMode::Head, &hung)
            .expect("content is still served");
        assert_eq!(body["diff"]["error"], "git_timed_out", "{body}");
        assert_eq!(body["content"], "pub fn one() {}\n");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline held"
        );
    }
}
