//! Tests for proving an account-only pin's token (#8510).
//!
//! Why: the proof decides which GitHub account a session and a daemon lookup
//! act as. Every arm runs against temp dirs and table fakes: no test runs
//! `gh`, reads a keyring, or makes a network call (the 2026-09-24 incident).
//! The fakes answer only for the exact dir, host, login, API base URL and
//! token they were given, so an implementation that probes the wrong dir or
//! hardcodes `github.com` gets no answer and fails.
//! Test: itself.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::core::gh_account_dir::{
    AccountDirSources, ensure_config_version, refuse_unmigrated_config,
};
use crate::core::gh_account_proof::{
    AccountProver, CliTokenProbe, GhTokenProbe, GhUserCheck, HttpUserCheck, ProvenToken,
    api_base_url, identity_token_vars, parse_user_login, prove_account_token, token_var_for,
};

/// The repository every github.com arm pins.
pub(crate) const ORIGIN: &str = "https://github.com/duettoresearch/jev-matching";

/// A `gh auth token` lookup: `(dir, host, login)`.
type ProbeKey = (PathBuf, String, String);

/// `gh auth token` answers keyed by `(dir, host, login)`; records every call.
#[derive(Default)]
pub(crate) struct TableProbe {
    answers: Vec<(ProbeKey, Result<String, String>)>,
    calls: RefCell<Vec<ProbeKey>>,
}

impl TableProbe {
    /// Answer `gh auth token --hostname host -u login` under `dir`.
    pub(crate) fn answer(
        mut self,
        dir: &Path,
        host: &str,
        login: &str,
        answer: Result<&str, &str>,
    ) -> Self {
        let key = (dir.to_path_buf(), host.to_string(), login.to_string());
        let answer = answer.map(str::to_string).map_err(str::to_string);
        self.answers.push((key, answer));
        self
    }

    /// Every `(dir, host, login)` the probe was asked for, in order.
    pub(crate) fn calls(&self) -> Vec<(PathBuf, String, String)> {
        self.calls.borrow().clone()
    }
}

impl GhTokenProbe for TableProbe {
    fn token(&self, dir: &Path, host: &str, login: &str) -> Result<String, String> {
        let key = (dir.to_path_buf(), host.to_string(), login.to_string());
        self.calls.borrow_mut().push(key.clone());
        self.answers
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| Err(format!("no token scripted for {} on {host}", dir.display())))
    }
}

/// `GET /user` answers keyed by `(api_base, token)`; records every call.
#[derive(Default)]
pub(crate) struct TableCheck {
    answers: Vec<((String, String), Result<String, String>)>,
    calls: RefCell<Vec<(String, String)>>,
}

impl TableCheck {
    /// Answer `GET <api_base>/user` sent with `token`.
    pub(crate) fn answer(
        mut self,
        api_base: &str,
        token: &str,
        answer: Result<&str, &str>,
    ) -> Self {
        let key = (api_base.to_string(), token.to_string());
        let answer = answer.map(str::to_string).map_err(str::to_string);
        self.answers.push((key, answer));
        self
    }

    /// Every `(api_base, token)` the check was asked for, in order.
    pub(crate) fn calls(&self) -> Vec<(String, String)> {
        self.calls.borrow().clone()
    }
}

impl GhUserCheck for TableCheck {
    fn login(&self, api_base: &str, token: &str) -> Result<String, String> {
        let key = (api_base.to_string(), token.to_string());
        self.calls.borrow_mut().push(key.clone());
        self.answers
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| Err(format!("no /user answer scripted for {api_base}")))
    }
}

/// A gh config dir gh would not migrate: `config.yml` declares `version: "1"`.
pub(crate) fn migrated_dir(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("config dir");
    std::fs::write(dir.join("config.yml"), "version: \"1\"\n").expect("config.yml");
    dir.to_path_buf()
}

/// Sources naming only a static dir and the daemon's own dir.
fn sources(static_dir: Option<&Path>, own: Option<&Path>) -> AccountDirSources {
    AccountDirSources {
        static_config_dir: static_dir.map(Path::to_path_buf),
        state_root: None,
        own_config_dir: own.map(Path::to_path_buf),
    }
}

/// The reasons a refused proof carries, joined.
fn refusal(result: Result<ProvenToken, Vec<String>>) -> String {
    result.expect_err("the proof must refuse").join("; ")
}

/// 🔴 #8510 MEDIUM: a failed first candidate never stops the loop; the second
/// candidate's proven token is used.
/// Test: itself.
#[test]
fn the_second_candidate_is_used_when_the_first_fails() {
    let root = tempfile::tempdir().expect("tempdir");
    let first = migrated_dir(&root.path().join("static"));
    let second = migrated_dir(&root.path().join("own"));
    let probe = TableProbe::default()
        .answer(
            &first,
            "github.com",
            "octo-pinned",
            Err("exit status 1: no token"),
        )
        .answer(&second, "github.com", "octo-pinned", Ok("tok-2"));
    let check = TableCheck::default().answer("https://api.github.com", "tok-2", Ok("octo-pinned"));
    let prover = AccountProver {
        sources: &sources(Some(&first), Some(&second)),
        probe: &probe,
        check: &check,
        cache: None,
    };
    let proven = prover
        .prove("octo-pinned", ORIGIN)
        .expect("the second must prove");
    assert_eq!(proven, ProvenToken::for_test("github.com", "tok-2"));
    assert_eq!(probe.calls().len(), 2, "both candidates are asked");
}

/// 🔴 #8510 CRITICAL: a token `gh` returned for the login is refused when
/// `GET /user` says it is another account's — even under a dir whose
/// `hosts.yml` names the login as the active user.
/// Test: itself.
#[test]
fn a_token_for_another_account_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    let own = migrated_dir(root.path());
    std::fs::write(
        own.join("hosts.yml"),
        "github.com:\n    user: octo-pinned\n",
    )
    .expect("hosts");
    let probe = TableProbe::default().answer(&own, "github.com", "octo-pinned", Ok("tok-global"));
    let check =
        TableCheck::default().answer("https://api.github.com", "tok-global", Ok("octo-other"));
    let err = refusal(prove_account_token(
        &sources(None, Some(&own)),
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    ));
    assert!(
        err.contains("authenticates as 'octo-other'") && !err.contains("tok-"),
        "got: {err}"
    );
}

/// GitHub logins compare case-insensitively.
/// Test: itself.
#[test]
fn a_login_differing_only_in_case_is_proven() {
    let root = tempfile::tempdir().expect("tempdir");
    let own = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&own, "github.com", "octo-pinned", Ok("tok-b"));
    let check = TableCheck::default().answer("https://api.github.com", "tok-b", Ok("Octo-Pinned"));
    prove_account_token(
        &sources(None, Some(&own)),
        "octo-pinned",
        ORIGIN,
        &probe,
        &check,
    )
    .expect("case must not matter");
}

/// 🔴 #8510 MEDIUM: the token lookup succeeds but `GET /user` fails. Every
/// failure is "not proven", and the reason keeps the failure's own text — a
/// `.unwrap_or_default()` would drop it and read an empty login instead.
/// Test: itself.
#[test]
fn a_failed_user_check_is_not_proof() {
    for failure in [
        "GET https://api.github.com/user did not answer in time",
        "GET https://api.github.com/user answered HTTP 401",
        "GET https://api.github.com/user answered a body that is not JSON",
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let own = migrated_dir(root.path());
        let probe = TableProbe::default().answer(&own, "github.com", "octo-pinned", Ok("tok-b"));
        let check = TableCheck::default().answer("https://api.github.com", "tok-b", Err(failure));
        let err = refusal(prove_account_token(
            &sources(None, Some(&own)),
            "octo-pinned",
            ORIGIN,
            &probe,
            &check,
        ));
        assert!(
            err.contains("could not be proven") && err.contains(failure) && !err.contains("tok-"),
            "got: {err}"
        );
    }
}

/// 🔴 #8510 HIGH: a candidate whose `config.yml` declares no version is
/// refused BEFORE `gh` runs in it: gh would migrate it and could overwrite a
/// keyring slot.
/// Test: itself.
#[test]
fn a_candidate_without_a_config_version_is_refused_before_gh_runs() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join("config.yml"), "git_protocol: https\n").expect("config");
    let probe = TableProbe::default().answer(root.path(), "github.com", "octo-pinned", Ok("tok"));
    let err = refusal(prove_account_token(
        &sources(None, Some(root.path())),
        "octo-pinned",
        ORIGIN,
        &probe,
        &TableCheck::default(),
    ));
    assert!(
        err.contains("declares no `version: \"1\"`") && err.contains("tm never runs gh"),
        "got: {err}"
    );
    assert!(
        probe.calls().is_empty(),
        "gh must never run: {:?}",
        probe.calls()
    );
}

/// 🔴 #8510 HIGH: no `config.yml` at all is refused before `gh` runs.
/// Test: itself.
#[test]
fn a_candidate_without_a_config_yml_is_refused_before_gh_runs() {
    let root = tempfile::tempdir().expect("tempdir");
    let probe = TableProbe::default();
    let err = refusal(prove_account_token(
        &sources(Some(root.path()), None),
        "octo-pinned",
        ORIGIN,
        &probe,
        &TableCheck::default(),
    ));
    assert!(err.contains("could not be read"), "got: {err}");
    assert!(
        probe.calls().is_empty(),
        "gh must never run: {:?}",
        probe.calls()
    );
}

/// A version other than `"1"` is refused too: tm knows only that one is safe.
/// Test: itself.
#[test]
fn a_candidate_with_an_unknown_config_version_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join("config.yml"), "version: 2\n").expect("config");
    let err = refuse_unmigrated_config(root.path()).expect_err("an unknown version refuses");
    assert!(err.contains("declares version '2'"), "got: {err}");
    std::fs::write(root.path().join("config.yml"), "version: 1\n").expect("config");
    refuse_unmigrated_config(root.path()).expect("an unquoted 1 is the same version");
}

/// 🔴 #8510 MEDIUM: an Enterprise Server origin is probed on its own host and
/// checked against its own API; the fakes answer nowhere else.
/// Test: itself.
#[test]
fn the_user_check_is_sent_to_the_hosts_api() {
    let root = tempfile::tempdir().expect("tempdir");
    let own = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&own, "ghe.corp", "octo-pinned", Ok("tok-ghe"));
    let check =
        TableCheck::default().answer("https://ghe.corp/api/v3", "tok-ghe", Ok("octo-pinned"));
    let proven = prove_account_token(
        &sources(None, Some(&own)),
        "octo-pinned",
        "https://ghe.corp/duettoresearch/jev-matching",
        &probe,
        &check,
    )
    .expect("the Enterprise Server token must prove");
    assert_eq!(proven.host, "ghe.corp");
    assert_eq!(
        probe.calls(),
        vec![(own, "ghe.corp".to_string(), "octo-pinned".to_string())]
    );
    assert_eq!(
        check.calls(),
        vec![("https://ghe.corp/api/v3".to_string(), "tok-ghe".to_string())]
    );
}

/// An origin with no parsable host proves nothing and asks nothing.
/// Test: itself.
#[test]
fn an_origin_with_no_host_is_refused() {
    let probe = TableProbe::default();
    let err = refusal(prove_account_token(
        &AccountDirSources::default(),
        "octo-pinned",
        "local-checkout",
        &probe,
        &TableCheck::default(),
    ));
    assert!(
        err.contains("cannot tell which gh host serves it"),
        "got: {err}"
    );
    assert!(probe.calls().is_empty());
}

/// Each host class has its own API base and token variable (#8510 HIGH).
/// Test: itself.
#[test]
fn api_base_url_and_token_var_follow_the_host_class() {
    for (host, api, var) in [
        ("github.com", "https://api.github.com", "GH_TOKEN"),
        ("octo.ghe.com", "https://api.octo.ghe.com", "GH_TOKEN"),
        ("ghe.corp", "https://ghe.corp/api/v3", "GH_ENTERPRISE_TOKEN"),
    ] {
        assert_eq!(api_base_url(host), api, "{host}");
        assert_eq!(token_var_for(host), var, "{host}");
    }
    assert_eq!(
        crate::core::gh_account_proof::origin_host("git@ssh.github.com:acme/widget.git"),
        Ok("github.com".to_string())
    );
}

/// Only a `200` whose JSON body carries a non-empty `login` proves anything,
/// and a refusal never echoes the body.
/// Test: itself.
#[test]
fn parse_user_login_accepts_only_a_200_with_a_login() {
    let url = "https://api.github.com/user";
    assert_eq!(
        parse_user_login(url, 200, r#"{"login":"octo-pinned","id":1}"#),
        Ok("octo-pinned".to_string())
    );
    for (status, body) in [
        (401, r#"{"login":"octo-pinned"}"#),
        (301, ""),
        (200, "<html>tok-secret</html>"),
        (200, r#"{"login":""}"#),
        (200, r#"{"id":1}"#),
    ] {
        let err = parse_user_login(url, status, body).expect_err("not proof");
        assert!(!err.contains("tok-secret"), "the body leaked: {err}");
    }
}

/// A dir named by two sources is asked once.
/// Test: itself.
#[test]
fn a_dir_named_twice_is_probed_once() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let candidates = sources(Some(&dir), Some(&dir)).candidates("octo-pinned");
    assert_eq!(candidates, vec![Ok(dir)]);
}

/// 🔴 No token reaches `Debug` output or a `describe()` diagnostic.
/// Test: itself.
#[test]
fn a_proven_token_never_prints() {
    let proven = ProvenToken::for_test("ghe.corp", "tok-secret");
    assert!(!format!("{proven:?}").contains("tok-secret"));
    let env = crate::core::gh_identity::GhEnv::from_identity_vars(proven.identity_vars());
    assert!(!env.describe().contains("tok-secret"), "{}", env.describe());
}

/// `describe()` redacts `GH_ENTERPRISE_TOKEN` as it redacts `GH_TOKEN`.
/// Test: itself.
#[test]
fn describe_redacts_an_enterprise_token() {
    let vars = identity_token_vars(Some(("ghe.corp", "tok-ghe")));
    let described = crate::core::gh_identity::GhEnv::from_identity_vars(vars).describe();
    assert!(
        described.contains("GH_ENTERPRISE_TOKEN=<redacted>") && !described.contains("tok-ghe"),
        "{described}"
    );
}

/// 🔴 #8510 incident: the production seams refuse in a unit-test build, so a
/// test that reaches them by mistake runs no `gh` and sends no request.
/// Test: itself.
#[test]
fn the_production_seams_never_run_in_a_unit_test() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let err = CliTokenProbe
        .token(&dir, "github.com", "octo-pinned")
        .expect_err("no gh in a unit test");
    assert!(err.contains("never runs gh"), "{err}");
    let err = HttpUserCheck
        .login("https://api.github.com", "tok")
        .expect_err("no network in a unit test");
    assert!(err.contains("never calls the GitHub API"), "{err}");
}

/// 🔴 #8510 r6: a `config.yml` that opens with a `---` document marker gains
/// `version` inside that document. Prepending it above the marker made two
/// documents: gh read only the first, and the migration guard refused the dir.
/// Test: itself.
#[test]
fn ensure_config_version_keeps_a_leading_document_marker() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("config.yml");
    std::fs::write(
        &path,
        "# operator settings\n---\ngit_protocol: ssh\neditor: vim\n",
    )
    .expect("config");
    ensure_config_version(root.path()).expect("the version must be added");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(
        text,
        "# operator settings\n---\nversion: \"1\"\ngit_protocol: ssh\neditor: vim\n"
    );
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("one YAML document");
    assert_eq!(doc["git_protocol"].as_str(), Some("ssh"), "{text}");
    assert_eq!(doc["editor"].as_str(), Some("vim"), "{text}");
    refuse_unmigrated_config(root.path()).expect("the rewritten dir must pass the guard");
}
