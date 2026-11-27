//! #12028 P1g: plain runs of the shipped `trusty-architect-verify-claim`.
//!
//! The replay recordings run the library through `examples/replay_port.rs`;
//! these prove the shipped binary reads `local/architect.json` under the
//! Architect checkout (marked by `ARCHITECT.md`) from a working directory
//! below it, refuses with exit 1 and no writes and no command when there is
//! no checkout, and starts every gh child without `GH_TOKEN` or
//! `GITHUB_TOKEN`, with `GH_CONFIG_DIR` set per owner.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-architect-verify-claim");
const HEAD: &str = "1111111111aaaaaaaaaa2222222222bbbbbbbbbb";

/// A checkout with `ARCHITECT.md`, a config naming two owners (one with a
/// gh config dir, one without) and a `sub/` directory to run from; plus a
/// stub directory whose `gh` logs its environment to `<stubs>/gh-env-<pid>`.
fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    let co = tempfile::tempdir().unwrap();
    let root = co.path();
    std::fs::write(root.join("ARCHITECT.md"), "# Architect\n").unwrap();
    std::fs::create_dir_all(root.join("local")).unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    let config = format!(
        r#"{{"repos": {{
  "example-repo": {{"slug": "example-org/example-repo", "gh_config_dir": "{}/gh-one"}},
  "second": {{"slug": "example-two/second"}}}}}}"#,
        root.display()
    );
    std::fs::write(root.join("local/architect.json"), config).unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let gh = stubs.path().join("gh");
    let script = format!(
        "#!/bin/sh\nenv > '{}/gh-env-'$$\nprintf '%s' '{{\"headRefOid\": \"{HEAD}\"}}'\n",
        stubs.path().display()
    );
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    (co, stubs)
}

fn run_bin(cwd: &Path, stubs: &Path, event: &str, extra: &[(&str, &str)]) -> Output {
    // A cleared environment: no host variable reaches the run or its logs.
    let mut cmd = Command::new(BIN);
    cmd.arg(event)
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

/// Every environment a gh stub logged, one `Vec<line>` per call.
fn gh_envs(stubs: &Path) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = std::fs::read_dir(stubs)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("gh-env-"))
        .map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .collect();
    out.sort();
    out
}

/// The variable names of one logged environment, never the values.
fn keys(env: &[String]) -> Vec<&str> {
    env.iter().filter_map(|l| l.split('=').next()).collect()
}

fn event(repo: &str, pr: u32) -> String {
    format!(r#"{{"kind": "status", "repo": "{repo}", "pr": {pr}, "head": "{HEAD}"}}"#)
}

#[test]
fn shipped_verify_claim_checks_a_pr_from_a_foreign_directory() {
    let (co, stubs) = setup();
    let out = run_bin(
        &co.path().join("sub"),
        stubs.path(),
        &event("example-repo", 12),
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("MATCH pr 12 head {HEAD}\n"));
    assert_eq!(text(&out.stderr), "");
    assert_eq!(gh_envs(stubs.path()).len(), 1);
    // Read-only: the checkout holds what `setup` wrote and nothing else.
    let mut names: Vec<String> = std::fs::read_dir(co.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["ARCHITECT.md", "local", "sub"]);
}

#[test]
fn shipped_verify_claim_without_a_checkout_exits_1() {
    let (_co, stubs) = setup();
    let foreign = tempfile::tempdir().unwrap();
    let out = run_bin(
        foreign.path(),
        stubs.path(),
        &event("example-repo", 12),
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert_eq!(
        text(&out.stderr),
        "verify-claim.py: no Architect checkout (ARCHITECT.md) at or above the working \
         directory; set ARCHITECT_CONFIG\n"
    );
    assert!(gh_envs(stubs.path()).is_empty(), "no command may run");
    assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 0);
    // Bad input is refused before the checkout is needed, as the script does.
    let out = run_bin(foreign.path(), stubs.path(), "{", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).starts_with("BADINPUT invalid JSON: "));
}

#[test]
fn shipped_verify_claim_strips_tokens_from_gh_children() {
    let (co, stubs) = setup();
    let secrets = [
        ("GH_TOKEN", "not-a-real-value-1"),
        ("GITHUB_TOKEN", "not-a-real-value-2"),
        ("GH_CONFIG_DIR", "inherited-dir"),
    ];
    for (repo, pr) in [("example-org/example-repo", 21), ("example-two/second", 22)] {
        let out = run_bin(co.path(), stubs.path(), &event(repo, pr), &secrets);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), format!("MATCH pr {pr} head {HEAD}\n"));
    }
    let envs = gh_envs(stubs.path());
    assert_eq!(envs.len(), 2);
    for env in &envs {
        assert!(
            !env.iter()
                .any(|l| l.starts_with("GH_TOKEN=") || l.starts_with("GITHUB_TOKEN=")),
            "a token reached gh: {:?}",
            keys(env)
        );
        assert!(
            env.iter().any(|l| l.starts_with("PATH=")),
            "{:?}",
            keys(env)
        );
    }
    let dirs: Vec<&String> = envs
        .iter()
        .flatten()
        .filter(|l| l.starts_with("GH_CONFIG_DIR="))
        .collect();
    // The mapped owner gets its own dir; the other keeps the inherited one.
    let own = format!("GH_CONFIG_DIR={}/gh-one", co.path().display());
    assert_eq!(dirs.len(), 2, "{dirs:?}");
    assert!(dirs.iter().any(|d| **d == own), "{dirs:?}");
    assert!(
        dirs.iter().any(|d| *d == "GH_CONFIG_DIR=inherited-dir"),
        "{dirs:?}"
    );
}
