//! Unit tests for account-selected clone credentials (#7166).
//!
//! Why: split out alongside `account_clone.rs` (issue #7166 review
//! follow-up) so the production module stays under the 500-SLOC cap; the
//! tests themselves are unchanged in substance from their prior home in
//! `inproject/tests.rs`. Coverage: the shadow-then-set env shape built
//! against a `Command` shaped exactly like the real clone invocation (argv
//! AND env asserted, so the token-never-in-argv claim can actually fail),
//! the not-logged-in refusal, the account-mismatch refusal (the #7166
//! critic BLOCK this file exists to close), a verification-I/O-failure
//! propagation, and the matching-login happy path.
//! What: pure-function assertions over `account_clone_env_with`'s
//! injectable `resolve_token`/`verify_login` closures — no real `gh`
//! process, no PATH mutation, no network.
//! Test: this file IS the test module.

use super::*;

/// A resolved token becomes `GH_TOKEN`/`GH_USER` on the child, with every
/// inherited identity var removed FIRST — an ambient `GH_TOKEN` belonging to
/// a different account can never win over the explicit selection.
#[test]
fn account_clone_env_shadows_inherited_identity_and_sets_gh_token() {
    const CLONE_URL: &str = "https://github.com/duettoresearch/poc-hotel-supply";

    let env = account_clone_env_with(
        "bob-duetto",
        |account| {
            assert_eq!(account, "bob-duetto");
            Ok("minted-token".to_string())
        },
        |token| {
            assert_eq!(token, "minted-token");
            Ok("bob-duetto".to_string())
        },
    )
    .expect("resolver and verification both succeeded");

    assert_eq!(
        env.remove,
        clone_env_removal_set(),
        "must shadow the shared inherited-identity set PLUS the clone-local GH_HOST"
    );
    assert_eq!(
        env.set,
        vec![
            ("GH_TOKEN".to_string(), "minted-token".to_string()),
            ("GH_USER".to_string(), "bob-duetto".to_string()),
        ]
    );

    // Built the SAME shape `ensure_base_clone` builds — argv first, `env.apply`
    // after — so the argv assertion below actually exercises the real clone
    // command, not an argv-free stand-in that could never fail it.
    let base_path = std::path::Path::new("/tmp/does-not-need-to-exist/base");
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--no-local", CLONE_URL])
        .arg(base_path)
        .env("GH_TOKEN", "someone-elses-ambient-token")
        .env("GITHUB_TOKEN", "another-ambient-token")
        .env("GH_HOST", "ghe.example.com");
    env.apply(&mut cmd);

    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(
        envs.contains(&(
            std::ffi::OsStr::new("GH_TOKEN"),
            Some(std::ffi::OsStr::new("minted-token"))
        )),
        "GH_TOKEN must be the minted token, not the ambient one: {envs:?}"
    );
    assert!(
        envs.contains(&(std::ffi::OsStr::new("GITHUB_TOKEN"), None)),
        "the ambient GITHUB_TOKEN must be shadowed (removed): {envs:?}"
    );
    assert!(
        envs.contains(&(std::ffi::OsStr::new("GH_HOST"), None)),
        "the ambient GH_HOST must be shadowed (removed) — the clone URL is hardcoded to \
         github.com: {envs:?}"
    );

    // The token must never appear in argv OR the URL — only in the env `set` list.
    let argv: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        argv,
        vec![
            "clone",
            "--no-local",
            CLONE_URL,
            base_path.to_str().unwrap()
        ],
        "argv must be exactly the plain clone invocation, no credential appended"
    );
    assert!(
        argv.iter().all(|a| !a.contains("minted-token")),
        "the token must never appear in argv: {argv:?}"
    );
    assert!(
        !CLONE_URL.contains("minted-token"),
        "the token must never appear in the clone URL"
    );
}

/// A resolver failure (e.g. the account is not logged into `gh`) propagates
/// verbatim, naming the account — the "fail loud before cloning" contract.
/// `verify_login` must not even run: there is no token to verify.
#[test]
fn account_clone_env_propagates_a_resolver_failure_naming_the_account() {
    let err = account_clone_env_with(
        "not-logged-in",
        |account| {
            Err(format!(
                "`gh auth token -u {account}` failed: not logged in"
            ))
        },
        |_| panic!("verify_login must not run when token resolution already failed"),
    )
    .expect_err("a resolver failure must propagate");
    assert!(err.contains("not-logged-in"), "{err}");
}

/// #7166 critic BLOCK: `gh auth token -u <account>` can mint a DIFFERENT
/// account's token on a keyring-backed host. A mismatch between the
/// requested account and what `gh api user` reports for the minted token
/// must refuse loud, naming BOTH logins, before any clone runs.
#[test]
fn account_clone_env_refuses_a_token_minted_for_a_different_account() {
    let err = account_clone_env_with(
        "bob-duetto",
        |_| Ok("token-for-the-wrong-account".to_string()),
        |token| {
            assert_eq!(token, "token-for-the-wrong-account");
            Ok("bobmatnyc".to_string())
        },
    )
    .expect_err("a login mismatch must refuse");
    assert!(err.contains("bob-duetto"), "{err}");
    assert!(err.contains("bobmatnyc"), "{err}");
}

/// A `verify_login` I/O failure (e.g. `gh api user` itself could not run)
/// propagates rather than being treated as a silent match.
#[test]
fn account_clone_env_propagates_a_verification_failure() {
    let err = account_clone_env_with(
        "bob-duetto",
        |_| Ok("some-token".to_string()),
        |_| Err("gh api user failed: network error".to_string()),
    )
    .expect_err("a verification failure must propagate, not be swallowed");
    assert!(err.contains("bob-duetto"), "{err}");
}

/// A token that DOES belong to the requested account proceeds normally —
/// the verification step is not a blanket refusal, only a mismatch one.
#[test]
fn account_clone_env_accepts_a_token_that_matches() {
    let env = account_clone_env_with(
        "bob-duetto",
        |_| Ok("minted-token".to_string()),
        // GitHub logins are case-insensitive — the comparison must be too.
        |_| Ok("Bob-Duetto".to_string()),
    )
    .expect("a matching (case-insensitively) login must proceed");
    assert_eq!(
        env.set,
        vec![
            ("GH_TOKEN".to_string(), "minted-token".to_string()),
            ("GH_USER".to_string(), "bob-duetto".to_string()),
        ]
    );
}

// -----------------------------------------------------------------------
// #7166 review follow-up HIGH: `verify_token_login_with`'s bounded timeout.
// -----------------------------------------------------------------------

/// A `run` closure that outruns `timeout` produces a distinct, named
/// "timed out" error — never a silent hang, and never mistaken for `run`'s
/// own failure shape.
#[test]
fn verify_token_login_with_times_out_and_names_the_bound() {
    let err = verify_token_login_with(std::time::Duration::from_millis(20), || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        Ok("someone".to_string())
    })
    .expect_err("a stalled run must time out");
    assert!(err.contains("timed out"), "{err}");
}

/// A `run` closure that finishes well inside `timeout` returns its result
/// unmodified — the bound is a ceiling, not a delay.
#[test]
fn verify_token_login_with_returns_the_inner_result_when_fast() {
    let login = verify_token_login_with(std::time::Duration::from_secs(5), || {
        Ok("bobmatnyc".to_string())
    })
    .expect("a fast run must succeed");
    assert_eq!(login, "bobmatnyc");
}

/// A `run` closure that fails (fast) propagates ITS error, not a timeout —
/// the two failure shapes must stay distinguishable.
#[test]
fn verify_token_login_with_propagates_a_fast_failure_untouched() {
    let err = verify_token_login_with(std::time::Duration::from_secs(5), || {
        Err("gh api user failed: 401".to_string())
    })
    .expect_err("a fast failure must propagate");
    assert!(err.contains("401"), "{err}");
    assert!(!err.contains("timed out"), "{err}");
}
