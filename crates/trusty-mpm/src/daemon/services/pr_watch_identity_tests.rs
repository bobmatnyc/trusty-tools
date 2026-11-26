//! The PR watch's `gh` identity, through the production reader (#11673).
//!
//! Why: the loop tests drive a `FakeReader`, so they never saw that the
//! daemon's reader ran `gh` with launchd's bare environment and exited 4.
//! What: [`daemon_reader`] over a temp registry, with a stub `gh` first on
//! `PATH` that exits 4 unless `GH_TOKEN` or `GH_CONFIG_DIR` is set, inside a
//! credential sandbox with `GH_CONFIG_DIR` removed — launchd's environment.
//! Test: the tests below.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use trusty_common::credentials::test_sandbox::CredentialSandbox;

use super::{BusEmitter, DbatgLedger, daemon_reader};
use crate::core::pr_watch::gh::{IdentityError, PrReader};
use crate::core::pr_watch::table::{Table, WatchRow};
use crate::daemon::services::pr_watch_loop::{Ports, PrWatchLoop, Waker, unbound_summary};

const REPO: &str = "acme-labs/jev-matching";
const ORIGIN: &str = "https://github.com/acme-labs/jev-matching";

/// A stub `gh`: exit 4 unless an identity is bound; logs each call.
const STUB: &str = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/calls.log"
if [ -z "$GH_TOKEN" ] && [ -z "$GH_CONFIG_DIR" ]; then
  echo "To get started with GitHub CLI, please run:  gh auth login" >&2
  exit 4
fi
echo '{"state":"OPEN","headRefOid":"abc123","baseRefName":"main","statusCheckRollup":[]}'
"#;

/// The sandbox, the stub's directory and a registry directory.
struct Launchd {
    sandbox: CredentialSandbox,
    bin: PathBuf,
    registry: PathBuf,
}

impl Launchd {
    /// Enter launchd's environment: no credential, no `GH_CONFIG_DIR`, the
    /// stub `gh` first on `PATH`. Caller is `#[serial]` and holds the PATH lock.
    fn enter() -> Self {
        let mut sandbox = CredentialSandbox::enter();
        sandbox.remove("GH_CONFIG_DIR");
        let bin = sandbox.root().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        trusty_common::exec_stub::write_executable(&bin.join("gh"), STUB).expect("stub gh");
        let mut path = vec![bin.clone()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        sandbox.set("PATH", std::env::join_paths(path).expect("join PATH"));
        let registry = sandbox.root().join("registry");
        std::fs::create_dir_all(&registry).expect("registry dir");
        Self {
            sandbox,
            bin,
            registry,
        }
    }

    fn registry(&self, body: &str) {
        std::fs::write(self.registry.join("projects.json"), body).expect("projects.json");
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.bin.join("calls.log")).unwrap_or_default()
    }

    fn reader(&self) -> Arc<dyn PrReader> {
        Arc::new(daemon_reader(
            self.registry.clone(),
            Duration::from_secs(10),
        ))
    }
}

/// A registry pinning [`ORIGIN`] to the gh config dir `dir`.
fn pin_body(dir: &Path) -> String {
    format!(
        r#"{{"projects":{{"jev-matching":{{"name":"jev-matching","repo_url":"{ORIGIN}","default_branch":"main","gh_account":"acme-admin","github":{{"config_dir":"{}"}}}}}}}}"#,
        dir.display()
    )
}

/// 🔴 #11673 REGRESSION: a repo pinned in the registry is read through that
/// identity, under launchd's environment.
#[test]
#[serial_test::serial]
fn a_pinned_repo_reads_through_its_bound_identity() {
    let _path = crate::test_support::lock_path_env();
    let env = Launchd::enter();
    let gh_dir = env.sandbox.root().join("gh-acme");
    std::fs::create_dir_all(&gh_dir).expect("gh dir");
    std::fs::write(
        gh_dir.join("hosts.yml"),
        "github.com:\n    git_protocol: https\n    user: acme-admin\n",
    )
    .expect("hosts.yml");
    env.registry(&pin_body(&gh_dir));
    let snap = env
        .reader()
        .view(REPO, 7)
        .unwrap_or_else(|e| panic!("a bound identity must authenticate gh; got: {e:#}"));
    assert_eq!(snap.head, "abc123");
    assert!(
        env.calls()
            .contains("pr view 7 --repo acme-labs/jev-matching")
    );
}

/// 🔴 FAIL-CLOSED: a registry that cannot answer spawns no `gh` at all, and the
/// refusal is an [`IdentityError`], not an ordinary read failure.
#[test]
#[serial_test::serial]
fn a_registry_refusal_spawns_no_gh() {
    let _path = crate::test_support::lock_path_env();
    let env = Launchd::enter();
    env.registry("{}");
    let err = env
        .reader()
        .view(REPO, 7)
        .expect_err("a refused registry must not read");
    assert!(
        err.downcast_ref::<IdentityError>().is_some(),
        "want an IdentityError; got: {err:#}"
    );
    assert_eq!(env.calls(), "", "no gh may run without a resolved identity");
}

/// With nothing bound, `gh`'s exit 4 is an identity failure, reported once
/// for the tick — not one warn per PR, and not a healthy-looking tick.
#[test]
#[serial_test::serial]
fn an_unbound_tick_reports_one_identity_failure_for_every_row() {
    let _path = crate::test_support::lock_path_env();
    let env = Launchd::enter();
    env.registry(r#"{"projects":{}}"#);
    let table_dir = env.sandbox.root().join("table");
    let table = Table::at(&table_dir);
    for pr in 1..=3 {
        table.upsert(row(pr)).expect("add");
    }
    let ports = Ports {
        reader: env.reader(),
        emitter: Arc::new(BusEmitter {
            publisher: None,
            spool_dir: None,
        }),
        ledger: Arc::new(DbatgLedger),
        waker: Arc::new(NoWake),
    };
    // A sync test: the PATH lock is a std guard, never held across an await.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let report = rt.block_on(PrWatchLoop::new(table, ports, 60).tick(100));
    assert_eq!(
        (report.read, report.identity_failed, report.failed),
        (3, 3, 0)
    );
    assert_eq!(env.calls().lines().count(), 3, "one gh per row");
}

/// The tick's one line names every PR and the first cause.
#[test]
fn identity_failures_are_one_line_naming_every_pr() {
    assert_eq!(unbound_summary(&[]), None);
    let line = unbound_summary(&[
        ("o/r#1".into(), "exited 4: gh auth login".into()),
        ("o/r#2".into(), "exited 4: gh auth login".into()),
    ])
    .expect("a line");
    assert!(line.contains("2 read(s)") && line.contains("o/r#1, o/r#2"));
    assert!(line.contains("first: exited 4: gh auth login") && !line.contains('\n'));
}

struct NoWake;

#[async_trait::async_trait]
impl Waker for NoWake {
    async fn wake(&self, _owner: &str, _line: &str) -> anyhow::Result<()> {
        anyhow::bail!("no wake expected")
    }
}

fn row(pr: u64) -> WatchRow {
    WatchRow {
        repo: REPO.into(),
        pr,
        head: "h1".into(),
        base: "main".into(),
        required: vec![],
        owner: "pm-a".into(),
        project: None,
        verdict: None,
        emitted_key: None,
        ledger_key: None,
        last_woken_key: None,
        seq: 0,
        added_at: "2026-11-25T00:00:00Z".into(),
    }
}
