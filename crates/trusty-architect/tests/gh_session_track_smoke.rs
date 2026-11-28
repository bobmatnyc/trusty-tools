//! #12087: plain runs of the shipped `trusty-architect-gh-session-track`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! these prove the shipped binary finds the Architect checkout (marked by
//! `ARCHITECT.md`) from a working directory below it, takes the default repo
//! and the gh account directory from `local/architect.json`, writes the
//! default store `<checkout>/local/owner-queue` (or, with `--dry-run`,
//! nothing), refuses with exit 1, no gh call and no write when there is no
//! checkout, and starts every gh child without `GH_TOKEN` or `GITHUB_TOKEN`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-gh-session-track");

/// A checkout with `ARCHITECT.md`, a config whose one repo names a gh
/// config dir that exists, a session `alpha` in the default store and a
/// `sub/` directory to run from; plus a stub directory whose `gh` logs its
/// argv and environment and answers one open PR labelled `ws/alpha`.
fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let co = tempfile::tempdir().unwrap();
    let root = co.path();
    std::fs::write(root.join("ARCHITECT.md"), "# Architect\n").unwrap();
    std::fs::create_dir_all(root.join("local/owner-queue/sessions")).unwrap();
    std::fs::create_dir_all(root.join("gh-one")).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let config = format!(
        r#"{{"repos": {{"example-repo": {{"slug": "example-org/example-repo", "gh_config_dir": "{}/gh-one"}}}}}}"#,
        root.display()
    );
    std::fs::write(root.join("local/architect.json"), config).unwrap();
    let doc = "{\n  \"name\": \"alpha\",\n  \"project\": \"sample-app\",\n  \"version\": 1\n}\n";
    std::fs::write(root.join("local/owner-queue/sessions/alpha.json"), doc).unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let gh = stubs.path().join("gh");
    let log = stubs.path().display();
    let script = format!(
        "#!/bin/sh\nenv > '{log}/gh-env-'$$\necho \"$*\" >> '{log}/gh-argv'\n\
         case \"$1 $*\" in\n  api*) echo '[[]]' ;;\n  *'--state open -L 200'*) \
         echo '[{{\"number\": 7, \"url\": \"u\", \"labels\": [{{\"name\": \"ws/alpha\"}}], \"mergeStateStatus\": \"CLEAN\"}}]' ;;\n  \
         *) echo '[]' ;;\nesac\n"
    );
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    (co, stubs)
}

fn run_bin(cwd: &Path, stubs: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    // A cleared environment: no host variable reaches the run or its logs.
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", stubs.display()));
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("the binary runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn gh_argv(stubs: &Path) -> Vec<String> {
    std::fs::read_to_string(stubs.join("gh-argv"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every environment a gh stub logged, one `Vec<line>` per call.
fn gh_envs(stubs: &Path) -> Vec<Vec<String>> {
    std::fs::read_dir(stubs)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("gh-env-"))
        .map(|e| {
            let t = std::fs::read_to_string(e.path()).unwrap();
            t.lines().map(str::to_owned).collect()
        })
        .collect()
}

fn store_files(co: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![co.join("local/owner-queue")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                let rel = e
                    .path()
                    .strip_prefix(co)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

const ALPHA_LINE: &str = "alpha: PRs #7 ready (merged 0/24h 0/7d) | issues -\n";

#[test]
fn shipped_gh_session_track_dry_run_from_a_foreign_directory() {
    let (co, stubs) = setup();
    let out = run_bin(&co.path().join("sub"), stubs.path(), &["--dry-run"], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with(ALPHA_LINE),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(text(&out.stderr), "");
    let argv = gh_argv(stubs.path());
    assert_eq!(argv.len(), 4, "{argv:?}");
    assert!(
        argv[0].starts_with("pr list -R example-org/example-repo --state open"),
        "{argv:?}"
    );
    assert_eq!(
        store_files(co.path()),
        ["local/owner-queue/sessions/alpha.json"]
    );
}

#[test]
fn shipped_gh_session_track_writes_from_a_foreign_directory() {
    let (co, stubs) = setup();
    let out = run_bin(&co.path().join("sub"), stubs.path(), &[], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with(ALPHA_LINE),
        "{}",
        text(&out.stdout)
    );
    let alpha =
        std::fs::read_to_string(co.path().join("local/owner-queue/sessions/alpha.json")).unwrap();
    assert!(alpha.contains("\"gh_at\": "), "{alpha}");
    assert!(alpha.contains("\"version\": 2"), "{alpha}");
    let meta =
        std::fs::read_to_string(co.path().join("local/owner-queue/meta/gh_track.json")).unwrap();
    assert!(
        meta.contains("\"repo\": \"example-org/example-repo\""),
        "{meta}"
    );
    let files = store_files(co.path());
    assert!(
        files.contains(&"local/owner-queue/changes.jsonl".to_owned()),
        "{files:?}"
    );
}

#[test]
fn shipped_gh_session_track_without_a_checkout_exits_1() {
    let (_co, stubs) = setup();
    let foreign = tempfile::tempdir().unwrap();
    let out = run_bin(foreign.path(), stubs.path(), &[], &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert_eq!(
        text(&out.stderr),
        "gh-session-track.py: no Architect checkout (ARCHITECT.md) at or above the working directory\n"
    );
    assert!(gh_argv(stubs.path()).is_empty(), "no gh call may run");
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 0);
    // A bad command line is refused before the checkout is needed.
    let out = run_bin(foreign.path(), stubs.path(), &["--bogus"], &[]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn shipped_gh_session_track_strips_tokens_from_gh() {
    let (co, stubs) = setup();
    let secrets = [
        ("GH_TOKEN", "not-a-real-value-1"),
        ("GITHUB_TOKEN", "not-a-real-value-2"),
        ("GH_CONFIG_DIR", "inherited-dir"),
    ];
    let out = run_bin(co.path(), stubs.path(), &["--dry-run"], &secrets);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let envs = gh_envs(stubs.path());
    assert_eq!(envs.len(), 4);
    let own = format!("GH_CONFIG_DIR={}/gh-one", co.path().display());
    for env in &envs {
        let tokens = env
            .iter()
            .filter(|l| l.starts_with("GH_TOKEN=") || l.starts_with("GITHUB_TOKEN="));
        assert_eq!(tokens.count(), 0, "a token reached gh");
        let dirs: Vec<&String> = env
            .iter()
            .filter(|l| l.starts_with("GH_CONFIG_DIR="))
            .collect();
        assert_eq!(dirs, [&own]);
    }
}
