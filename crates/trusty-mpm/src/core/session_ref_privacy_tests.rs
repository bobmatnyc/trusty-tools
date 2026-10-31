//! Unit tests for the session-ref privacy gate (See #10614).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::*;

/// A lookup failure and garbled output are `Unknown`, never `Private`.
/// Test: itself.
#[test]
fn gh_output_maps_to_visibility() {
    let cases: [(Result<String, String>, RepoVisibility); 5] = [
        (Ok("true\n".into()), RepoVisibility::Private),
        (Ok("false".into()), RepoVisibility::Public("o/r".into())),
        (
            Ok("null".into()),
            RepoVisibility::Unknown("`gh api` answered \"null\"".into()),
        ),
        (
            Err("HTTP 404".into()),
            RepoVisibility::Unknown("HTTP 404".into()),
        ),
        (
            Err("not logged in".into()),
            RepoVisibility::Unknown("not logged in".into()),
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            visibility_from_gh("o/r", input.clone()),
            expected,
            "{input:?}"
        );
    }
}

/// Only a confirmed private repository lets the publish run.
/// Test: itself.
#[test]
fn only_a_private_repo_is_allowed() {
    assert_eq!(RepoVisibility::Private.refusal(), None);
    let public = RepoVisibility::Public("o/r".into())
        .refusal()
        .unwrap_or_default();
    assert!(public.contains("o/r is a public repository"), "{public}");
    let unknown = RepoVisibility::Unknown("HTTP 404".into())
        .refusal()
        .unwrap_or_default();
    assert!(
        unknown.contains("could not be confirmed as private"),
        "{unknown}"
    );
}

/// The home directory becomes `~`; a missing home changes nothing.
/// Test: itself.
#[test]
fn home_paths_are_redacted() {
    let text = "worktree /home/ada/src/x and /home/ada/.trusty-mpm/s.md";
    assert_eq!(
        redact_home(text, Some(Path::new("/home/ada/"))),
        "worktree ~/src/x and ~/.trusty-mpm/s.md"
    );
    assert_eq!(redact_home(text, None), text);
    // #10614: a home is a whole path segment, never a prefix of another one.
    let neighbours = "/home/adam/x, /home/ada.old/y, /home/ada_2, /home/ada. /home/ada:/z";
    assert_eq!(
        redact_home(neighbours, Some(Path::new("/home/ada"))),
        "/home/adam/x, /home/ada.old/y, /home/ada_2, ~. ~:/z"
    );
    assert_eq!(
        redact_home("cd /home/ada", Some(Path::new("/home/ada"))),
        "cd ~"
    );
    assert_eq!(redact_home(text, Some(Path::new("/"))), text);
}

/// The id is deterministic, ref-safe, and carries no part of the input.
/// Test: itself.
#[test]
fn anonymous_user_id_is_stable_and_non_personal() {
    let id = anonymous_user_id("Ada Lovelace");
    assert_eq!(id, anonymous_user_id("Ada Lovelace"));
    assert_ne!(id, anonymous_user_id("octocat"));
    assert_eq!(id.len(), 13);
    assert!(!id.contains("Ada"), "{id}");
    assert_eq!(
        crate::core::session_ref_publish::sanitize_ref_component(&id).as_deref(),
        Some(id.as_str())
    );
}

/// Run `git -C <dir> <args>`, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = trusty_common::git::command_in(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "fixture `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 🔴 #10614 REGRESSION: `git push origin` pushes to every push URL, so one
/// private URL does not vouch for a second public or non-GitHub one.
/// Test: itself.
#[test]
fn every_push_url_must_be_private() {
    let lookup = |owner_repo: &str| -> Result<String, String> {
        Ok(if owner_repo == "o/private" {
            "true"
        } else {
            "false"
        }
        .to_string())
    };
    let cases = [
        (
            "https://github.com/o/public.git",
            RepoVisibility::Public("o/public".into()),
        ),
        (
            "https://example.com/o/r.git",
            RepoVisibility::Unknown("origin push host example.com is not github.com".into()),
        ),
        ("https://github.com/o/private.git", RepoVisibility::Private),
    ];
    for (second, expected) in cases {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo = tmp.path();
        git(repo, &["init", "--initial-branch=main"]);
        git(
            repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/o/private.git",
            ],
        );
        for url in ["https://github.com/o/private.git", second] {
            git(repo, &["config", "--add", "remote.origin.pushurl", url]);
        }
        assert_eq!(
            origin_visibility_with(repo, &[], lookup),
            expected,
            "second push URL {second}"
        );
    }
    assert_eq!(
        visibility_of_urls(&[], lookup),
        RepoVisibility::Unknown("origin has no push URL".into())
    );
}

/// #10614: the lookup names github.com and drops an ambient enterprise host or
/// token, even one the push env carries.
/// Test: itself.
#[test]
fn the_lookup_targets_github_com() {
    let env = [("GH_HOST".to_string(), "ghe.example.invalid".to_string())];
    let cmd = gh_visibility_command("o/r", &env);
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "api",
            "--hostname",
            "github.com",
            "repos/o/r",
            "--jq",
            ".private"
        ]
    );
    for key in ["GH_HOST", "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"] {
        assert!(
            cmd.get_envs().any(|(k, v)| k == key && v.is_none()),
            "{key} must be removed: {:?}",
            cmd.get_envs().collect::<Vec<_>>()
        );
    }
}

/// #10614: a lookup that hangs is killed and reaped within its bound, and
/// reads as a failure — so the publish is skipped.
/// Test: itself.
#[test]
fn a_hung_lookup_is_killed_and_reaped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let pid_file = tmp.path().join("pid");
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg("echo $$ > \"$0\"; exec sleep 30")
        .arg(&pid_file);
    let started = Instant::now();
    let result = run_lookup(cmd, Duration::from_millis(500));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    let err = result.expect_err("a hung lookup is not an answer");
    assert!(err.contains("did not answer"), "{err}");
    assert!(matches!(
        visibility_from_gh("o/r", Err(err)),
        RepoVisibility::Unknown(_)
    ));
    let pid = std::fs::read_to_string(&pid_file).expect("the child wrote its pid");
    // `kill -0` succeeds on a live process AND on an unreaped zombie.
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("kill");
    assert!(
        !alive.success(),
        "pid {} was left running or unreaped",
        pid.trim()
    );
}
