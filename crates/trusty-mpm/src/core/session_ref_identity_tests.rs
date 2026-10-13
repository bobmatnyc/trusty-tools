//! Tests for the session-ref git identity resolver (#9160).
//!
//! Why: the resolver decides whether a session-ref fetch or push runs as the
//! project's pinned account, as the ambient credential, or not at all. Every
//! arm is driven through a real git checkout's origin, a `projects.json`
//! written by hand, and fake provers: no test runs `gh`, reads a keyring, or
//! leaves the machine.
//! Test: itself.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::{
    IdentitySeams, SessionRefGitEnv, SessionRefOp, Transport, session_ref_git_env_with, transport,
};
use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_proof::ProvenToken;
use crate::core::gh_org_accounts::OrgAccounts;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// The https origin every pinned fixture uses.
const ORIGIN: &str = "https://github.com/acme-9160/widget.git";

/// The fixture token; it must never appear in any text a test can see.
const TOKEN: &str = "tok-fixture-9160-secret";

/// A checkout whose `origin` is `origin`, plus a registry dir beside it.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new(origin: &str) -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["remote", "add", "origin", origin],
        ] {
            let out = trusty_common::git::command_in(&repo)
                .args(&args)
                .output()
                .expect("git");
            assert!(out.status.success(), "fixture `git {args:?}` failed");
        }
        Self { tmp, repo }
    }

    fn registry_dir(&self) -> PathBuf {
        self.tmp.path().join("registry")
    }

    /// Pin `ORIGIN` to `body`'s fields (a JSON object fragment).
    fn pin(&self, fields: &str) {
        let dir = self.registry_dir();
        std::fs::create_dir_all(&dir).expect("registry dir");
        std::fs::write(
            dir.join("projects.json"),
            format!(
                r#"{{"projects":{{"widget":{{"name":"widget","repo_url":"{ORIGIN}","default_branch":"main",{fields}}}}}}}"#
            ),
        )
        .expect("projects.json");
    }

    /// Resolve with an empty org map and `prove`, recording every login asked.
    fn resolve(
        &self,
        load: &dyn Fn() -> Result<OrgAccounts, crate::core::gh_org_accounts::OrgAccountsError>,
        prove: &dyn Fn(&PinnedGhIdentity, &str, &str) -> Result<ProvenToken, String>,
    ) -> SessionRefGitEnv {
        self.resolve_op(SessionRefOp::Fetch, load, prove)
    }

    /// [`Self::resolve`] for `op`.
    fn resolve_op(
        &self,
        op: SessionRefOp,
        load: &dyn Fn() -> Result<OrgAccounts, crate::core::gh_org_accounts::OrgAccountsError>,
        prove: &dyn Fn(&PinnedGhIdentity, &str, &str) -> Result<ProvenToken, String>,
    ) -> SessionRefGitEnv {
        let registry_dir = self.registry_dir();
        session_ref_git_env_with(
            &self.repo,
            op,
            &IdentitySeams {
                registry_dir: &registry_dir,
                state_root: self.tmp.path(),
                aliases: &SshHostAliases::empty(),
                load,
                prove,
            },
        )
    }
}

/// An empty `[accounts]` table.
fn no_map() -> Result<OrgAccounts, crate::core::gh_org_accounts::OrgAccountsError> {
    Ok(OrgAccounts::default())
}

/// The value of `name` in `env`'s vars.
fn value_of(env: &SessionRefGitEnv, name: &str) -> Option<String> {
    env.vars()
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// A prover that proves `TOKEN` for every login and records who was asked.
fn proving(
    asked: &RefCell<Vec<String>>,
) -> impl Fn(&PinnedGhIdentity, &str, &str) -> Result<ProvenToken, String> + '_ {
    move |_pinned, _origin, login| {
        asked.borrow_mut().push(login.to_string());
        Ok(ProvenToken::for_test("github.com", TOKEN))
    }
}

/// 🔴 #9160: a proven account pin injects the proven token, the login and a
/// git credential-helper pin for the origin's host — so git asks gh, not
/// osxkeychain.
/// Test: itself.
#[test]
fn a_proven_registry_pin_pins_gh_and_git() {
    let fx = Fixture::new(ORIGIN);
    fx.pin(r#""gh_account":"octo-pinned""#);
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&no_map, &proving(&asked));
    assert!(matches!(env, SessionRefGitEnv::Pinned { .. }), "{env:?}");
    assert_eq!(*asked.borrow(), ["octo-pinned"]);
    assert_eq!(value_of(&env, "GH_TOKEN").as_deref(), Some(TOKEN));
    assert_eq!(value_of(&env, "GH_USER").as_deref(), Some("octo-pinned"));
    assert_eq!(value_of(&env, "GIT_CONFIG_COUNT").as_deref(), Some("2"));
    assert_eq!(
        value_of(&env, "GIT_CONFIG_KEY_1").as_deref(),
        Some("credential.https://github.com.helper")
    );
    assert_eq!(
        value_of(&env, "GIT_CONFIG_VALUE_1").as_deref(),
        Some("!gh auth git-credential")
    );
}

/// 🔴 #9160: a proven config-dir pin sets `GH_CONFIG_DIR` and its identity
/// text names the login and the dir.
/// Test: itself.
#[test]
fn a_proven_config_dir_pin_names_the_dir() {
    let fx = Fixture::new(ORIGIN);
    let dir = fx.tmp.path().join("gh-octo");
    fx.pin(&format!(
        r#""gh_account":"octo-pinned","github":{{"config_dir":"{}"}}"#,
        dir.display()
    ));
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&no_map, &proving(&asked));
    let SessionRefGitEnv::Pinned { identity, .. } = &env else {
        panic!("a proven pin must pin: {env:?}");
    };
    assert!(identity.contains("'octo-pinned'"), "{identity}");
    assert!(
        identity.contains(&format!("GH_CONFIG_DIR={}", dir.display())),
        "{identity}"
    );
    assert_eq!(
        value_of(&env, "GH_CONFIG_DIR"),
        Some(dir.display().to_string())
    );
    assert!(
        env.annotate("fetch failed")
            .contains(&format!("git ran as {identity}")),
        "a pinned failure must name the identity"
    );
}

/// 🔴 #9160 (Fail-Open Check): an unproven pin is a refusal naming the
/// account, the dir and the proof's own reason — never the ambient credential.
/// Test: itself.
#[test]
fn an_unproven_pin_refuses_naming_account_and_dir() {
    let fx = Fixture::new(ORIGIN);
    let dir = fx.tmp.path().join("gh-octo");
    fx.pin(&format!(
        r#""gh_account":"octo-pinned","github":{{"config_dir":"{}"}}"#,
        dir.display()
    ));
    let refuse = |_: &PinnedGhIdentity, _: &str, _: &str| -> Result<ProvenToken, String> {
        Err("fixture: GET /user answered HTTP 401".to_string())
    };
    let env = fx.resolve(&no_map, &refuse);
    let reason = env.refusal().expect("an unproven pin must refuse");
    assert!(env.vars().is_empty(), "a refusal injects nothing: {env:?}");
    for want in [
        "'octo-pinned'",
        &format!("GH_CONFIG_DIR={}", dir.display()),
        "fixture: GET /user answered HTTP 401",
        "does not fall back to the ambient",
        "no session-ref network call was made",
    ] {
        assert!(reason.contains(want), "missing {want:?} in {reason}");
    }
}

/// 🔴 #9160 Q2: an origin no record or org entry pins keeps the ambient
/// credential, and the note says so and names the fix.
/// Test: itself.
#[test]
fn an_unpinned_origin_keeps_the_ambient_credential_and_says_so() {
    let fx = Fixture::new(ORIGIN);
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&no_map, &proving(&asked));
    let SessionRefGitEnv::Ambient { note } = &env else {
        panic!("an unpinned origin is ambient: {env:?}");
    };
    assert!(
        note.contains(&format!("ambient git credential, no gh pin for {ORIGIN}")),
        "{note}"
    );
    assert!(
        note.contains("--gh-account") && note.contains("[accounts]"),
        "{note}"
    );
    assert!(asked.borrow().is_empty(), "nothing is proven for no pin");
    assert!(env.vars().is_empty());
}

/// 🔴 #9160 / #9091: an origin only the `[accounts]` org map names is proven
/// and pinned as the mapped login.
/// Test: itself.
#[test]
fn an_org_mapped_origin_uses_the_mapped_login() {
    let fx = Fixture::new(ORIGIN);
    let load = || {
        OrgAccounts::from_toml(
            "[accounts]\nacme-9160 = \"octo-mapped\"\n",
            Path::new("/home/u/.trusty-mpm/config.toml"),
        )
    };
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&load, &proving(&asked));
    assert_eq!(*asked.borrow(), ["octo-mapped"]);
    assert_eq!(value_of(&env, "GH_USER").as_deref(), Some("octo-mapped"));
}

/// 🔴 #9160 (Fail-Open Check): a registry that cannot be read cannot say
/// "unpinned"; the resolver refuses and proves nothing.
/// Test: itself.
#[test]
fn an_unreadable_registry_refuses_without_proving() {
    let fx = Fixture::new(ORIGIN);
    std::fs::create_dir_all(fx.registry_dir().join("projects.json")).expect("blocker");
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&no_map, &proving(&asked));
    let reason = env.refusal().expect("an unreadable registry must refuse");
    assert!(
        reason.contains("no session-ref network call was made"),
        "{reason}"
    );
    assert!(asked.borrow().is_empty(), "nothing may be proven");
}

/// 🔴 #9160 Q4: an ssh origin reports that a gh pin cannot apply, injects
/// nothing, and is never presented as pinned.
/// Test: itself.
#[test]
fn an_ssh_origin_reports_that_the_pin_cannot_apply() {
    let fx = Fixture::new("git@github.com:acme-9160/widget.git");
    fx.pin(r#""gh_account":"octo-pinned""#);
    let asked = RefCell::new(Vec::new());
    let env = fx.resolve(&no_map, &proving(&asked));
    let SessionRefGitEnv::Ambient { note } = &env else {
        panic!("an ssh origin is never pinned: {env:?}");
    };
    assert!(
        note.contains("ssh remote") && note.contains("cannot apply"),
        "{note}"
    );
    assert!(env.annotate("push failed").contains("cannot apply"));
    assert!(asked.borrow().is_empty());
}

/// Set `key` to `value` in the fixture checkout's local git config.
fn git_config(fx: &Fixture, key: &str, value: &str) {
    let out = trusty_common::git::command_in(&fx.repo)
        .args(["config", key, value])
        .output()
        .expect("git");
    assert!(out.status.success(), "fixture `git config {key}` failed");
}

/// Assert `env` is the Q4 ambient "pin cannot apply" outcome and nothing was proven.
fn assert_pin_cannot_apply(env: &SessionRefGitEnv, asked: &RefCell<Vec<String>>) {
    let SessionRefGitEnv::Ambient { note } = env else {
        panic!("an ssh-bound transport must never be pinned: {env:?}");
    };
    assert!(
        note.contains("ssh") && note.contains("cannot apply"),
        "{note}"
    );
    assert!(env.vars().is_empty());
    assert!(asked.borrow().is_empty(), "nothing may be proven");
}

/// 🔴 #9160: an https origin that `url.<base>.insteadOf` rewrites to ssh is
/// fetched over ssh, so the pin cannot apply to a fetch or a publish.
/// Test: itself.
#[test]
fn an_insteadof_ssh_rewrite_is_never_pinned() {
    for op in [SessionRefOp::Fetch, SessionRefOp::Publish] {
        let fx = Fixture::new(ORIGIN);
        fx.pin(r#""gh_account":"octo-pinned""#);
        git_config(&fx, "url.git@github.com:.insteadOf", "https://github.com/");
        let asked = RefCell::new(Vec::new());
        let env = fx.resolve_op(op, &no_map, &proving(&asked));
        assert_pin_cannot_apply(&env, &asked);
    }
}

/// 🔴 #9160: a `pushInsteadOf` (or `pushurl`) to ssh leaves fetch on https
/// (still pinned) but moves the publish push onto ssh (never pinned).
/// Test: itself.
#[test]
fn a_push_url_rewritten_to_ssh_is_never_pinned_for_publish() {
    let rewrites = [
        ("url.git@github.com:.pushInsteadOf", "https://github.com/"),
        (
            "remote.origin.pushurl",
            "git@github.com:acme-9160/widget.git",
        ),
    ];
    for (key, value) in rewrites {
        let fx = Fixture::new(ORIGIN);
        fx.pin(r#""gh_account":"octo-pinned""#);
        git_config(&fx, key, value);
        let asked = RefCell::new(Vec::new());
        let fetch = fx.resolve_op(SessionRefOp::Fetch, &no_map, &proving(&asked));
        assert!(
            matches!(fetch, SessionRefGitEnv::Pinned { .. }),
            "{key}: {fetch:?}"
        );
        let asked = RefCell::new(Vec::new());
        let publish = fx.resolve_op(SessionRefOp::Publish, &no_map, &proving(&asked));
        assert_pin_cannot_apply(&publish, &asked);
    }
}

/// #9160: only `https://` takes the helper pin; ssh and scp syntax are ssh;
/// a local path is neither.
/// Test: itself.
#[test]
fn transport_classifies_https_ssh_and_local_origins() {
    assert_eq!(transport("https://github.com/a/b.git"), Transport::Https);
    assert_eq!(transport("ssh://git@github.com/a/b.git"), Transport::Ssh);
    assert_eq!(transport("git@github-alias:a/b.git"), Transport::Ssh);
    assert_eq!(transport("/srv/git/remote.git"), Transport::Other);
    assert_eq!(transport("file:///srv/git/remote.git"), Transport::Other);
}

/// 🔴 #9160: no token reaches the `Debug` text, an annotated error, or the
/// log, in the pinned arm or the refused arm.
/// Test: itself.
#[test]
fn session_ref_identity_errors_never_carry_a_token() {
    let fx = Fixture::new(ORIGIN);
    fx.pin(r#""gh_account":"octo-pinned""#);
    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let asked = RefCell::new(Vec::new());
    let shown = tracing::subscriber::with_default(subscriber, || {
        let pinned = fx.resolve(&no_map, &proving(&asked));
        assert_eq!(value_of(&pinned, "GH_TOKEN").as_deref(), Some(TOKEN));
        pinned.log(&fx.repo, "fetch");
        let refuse = |_: &PinnedGhIdentity, _: &str, _: &str| -> Result<ProvenToken, String> {
            Err("fixture refusal".to_string())
        };
        let refused = fx.resolve(&no_map, &refuse);
        refused.log(&fx.repo, "fetch");
        format!(
            "{pinned:?}\n{refused:?}\n{}\n{}",
            pinned.annotate("x"),
            refused.annotate("x")
        )
    });
    let log = buffer.text();
    assert!(!shown.contains(TOKEN), "a token reached the text: {shown}");
    assert!(!log.contains(TOKEN), "a token reached the log: {log}");
    assert!(
        log.contains("fixture refusal"),
        "the refusal must be logged: {log}"
    );
}

/// 🔴 #9160: a second catch-up inside the proof TTL asks neither gh nor
/// GitHub again — the resolver's prover is the cached spawn prover.
/// Test: itself.
#[test]
fn a_proven_identity_is_reused_across_two_resolutions() {
    use crate::core::gh_account_dir::AccountDirSources;
    use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
    use crate::core::gh_account_proof::{AccountProver, PROOF_TTL, ProofCache};

    let fx = Fixture::new(ORIGIN);
    fx.pin(r#""gh_account":"octo-pinned""#);
    let dir = migrated_dir(&fx.tmp.path().join("gh-own"));
    let probe = TableProbe::default().answer(&dir, "github.com", "octo-pinned", Ok(TOKEN));
    let check = TableCheck::default().answer("https://api.github.com", TOKEN, Ok("octo-pinned"));
    let cache = ProofCache::new(PROOF_TTL);
    let sources = AccountDirSources {
        own_config_dir: Some(dir.clone()),
        ..AccountDirSources::default()
    };
    let prove = |_: &PinnedGhIdentity, origin: &str, login: &str| {
        let prover = AccountProver {
            sources: &sources,
            probe: &probe,
            check: &check,
            cache: Some(&cache),
        };
        crate::core::gh_account::spawn_proof(&prover, login, origin, &SshHostAliases::empty())
    };
    for _ in 0..2 {
        let env = fx.resolve(&no_map, &prove);
        assert_eq!(
            value_of(&env, "GH_TOKEN").as_deref(),
            Some(TOKEN),
            "{env:?}"
        );
    }
    assert_eq!(probe.calls().len(), 1, "gh ran again: {:?}", probe.calls());
    assert_eq!(check.calls().len(), 1, "GitHub was asked again");
}

/// 🔴 #9160 Q3: the ref's `<user-id>` half still comes from the ambient gh
/// login, else `git config user.name` — a pin changes who pushes, never the
/// ref key, so existing refs are not orphaned.
/// Test: itself.
#[test]
fn resolve_user_id_still_reads_the_active_account() {
    use crate::core::session_ref_publish::{resolve_user_id, sanitize_ref_component};
    let fx = Fixture::new(ORIGIN);
    fx.pin(r#""gh_account":"octo-pinned""#);
    let out = trusty_common::git::command_in(&fx.repo)
        .args(["config", "user.name", "q3-fixture"])
        .output()
        .expect("git");
    assert!(out.status.success());
    let expected = crate::core::gh_account::gh_account_status_local()
        .and_then(|status| status.active)
        .filter(|login| !login.contains('@'))
        .and_then(|login| sanitize_ref_component(&login))
        .unwrap_or_else(|| "q3-fixture".to_string());
    assert_eq!(
        resolve_user_id(&fx.repo).as_deref(),
        Some(expected.as_str())
    );
    assert_ne!(
        expected, "octo-pinned",
        "the pin must not become the ref key"
    );
}

/// A cloneable in-memory log sink.
#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl LogBuffer {
    fn text(&self) -> String {
        let bytes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8(bytes).expect("utf-8 log")
    }
}

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
