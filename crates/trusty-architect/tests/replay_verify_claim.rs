//! #12028 P1g: the `verify-claim.py` recordings replay clean against the port.
//!
//! Recorded through the `driver.py` in the fixture directory, which points
//! `architect_config.load()` at the sandbox root and prints an uncaught
//! exception as its traceback's last line; `examples/replay_port.rs` mirrors
//! it. gh, git and curl are fakes declared in each recording, so a clean
//! replay also proves the port ran the same commands with the same argv, and,
//! every run being read-only, that it wrote nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use trusty_architect::replay::{Target, load, replay};

mod replay_support;

#[test]
fn verify_claim_recordings_replay_clean_against_the_port() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/replay/verify_claim");
    let target = Target {
        program: replay_support::build_example("replay_port"),
        prefix: vec!["verify_claim".to_owned()],
    };
    let parent = BTreeMap::from([("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())]);
    let (mut names, mut failed) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let rec = load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let found = replay(&rec, &target, &parent, Duration::from_secs(30)).unwrap();
        if !found.is_empty() {
            failed.push(format!("{}: {found:?}", path.display()));
        }
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    names.sort();
    assert_eq!(
        names,
        [
            "badinput-bad-kind.json",
            "badinput-checks-key.json",
            "badinput-ci-run-text.json",
            "badinput-claims-list.json",
            "badinput-crlf-file.json",
            "badinput-empty-file.json",
            "badinput-empty-stdin.json",
            "badinput-files-bool.json",
            "badinput-invalid-json-literal.json",
            "badinput-issue-reason-null.json",
            "badinput-issues-shape.json",
            "badinput-no-kind.json",
            "badinput-not-a-file.json",
            "badinput-not-object.json",
            "badinput-pr-bool.json",
            "badinput-pr-float.json",
            "badinput-repo-int.json",
            "ci-gh-fails.json",
            "ci-match.json",
            "ci-mismatch.json",
            "ci-no-claims.json",
            "config-bad-json.json",
            "config-env-file.json",
            "config-env-missing.json",
            "config-missing.json",
            "config-no-repos.json",
            "config-owner-two-dirs.json",
            "config-repos-not-object.json",
            "guard-refusal.json",
            "input-literal.json",
            "input-stdin.json",
            "issues-match.json",
            "issues-mismatch.json",
            "issues-no-account.json",
            "local-absent.json",
            "local-git-broken.json",
            "local-git-fails.json",
            "local-match.json",
            "local-mismatch.json",
            "local-no-checkout.json",
            "local-short.json",
            "no-checkable-claims.json",
            "no-repo.json",
            "offline-local.json",
            "offline.json",
            "pr-clearance-match.json",
            "pr-gh-fails.json",
            "pr-merged-match.json",
            "pr-mismatch.json",
            "pr-null-reply.json",
            "pr-reply-list.json",
            "pr-short-and-notsha.json",
            "pr-state-not-string.json",
            "published-bad-name.json",
            "published-match.json",
            "published-mismatch.json",
            "published-no-crate-key.json",
            "published-unreadable.json",
            "rate-limit-check-runs-unexpected.json",
            "rate-limit-rest-closed.json",
            "rate-limit-rest-fails.json",
            "rate-limit-rest-unexpected.json",
            "rate-limit-rest.json",
            "usage-help.json",
            "usage-no-args.json",
            "usage-two-args.json",
        ]
    );
}
