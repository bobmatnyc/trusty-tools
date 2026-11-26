//! Tests for `send_store.rs`, `secret_ref.rs` and `approval.rs` (#10423).

use std::sync::{Arc, Mutex};

use trusty_common::credentials::Secret;
use trusty_secrets::store::{MemoryBackend, NamesIndex, SecretStore};
use trusty_secrets::{SecretRef, SecretValue};

use super::approval::*;
use super::orgs::OrgsConfig;
use super::orgs_tests::INITIAL;
use super::secret_ref::*;
use super::send_store::*;
use super::slack_bot::{Posted, SendFailure, SlackPost};
use super::staged_send::*;
use super::staged_send_tests::{Audit, Sender, TEXT, approve, io, owner, staged};

/// Copies the table file as the post starts: the image a crash leaves.
struct CrashSender {
    table: std::path::PathBuf,
    image: std::path::PathBuf,
}
impl ChannelSender for CrashSender {
    fn send(&self, _r: &super::orgs::OrgRoute, _t: &str) -> Result<(), SendFailure> {
        std::fs::copy(&self.table, &self.image).expect("copy");
        Ok(())
    }
}

/// #10423 AC5: the table survives a restart; a send that was `Sending`
/// comes back `Unknown` and a second Approve never posts it.
#[test]
fn restart_turns_sending_into_unknown_and_never_reposts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonSendStore {
        path: table_path(dir.path()),
    };
    let (cfg, mut t, id, now) = staged();
    let audit = Audit::default();
    // An Unknown outcome and a still-Staged send, both persisted.
    let unknown = Sender::new(Err(SendFailure::Unknown("timeout".into())));
    t.decide(
        id,
        &owner(&cfg),
        &approve(),
        now,
        &unknown,
        &io(&audit, &store),
    )
    .expect("decided");
    let id2 = t
        .stage(&cfg, "bob-duetto", None, TEXT, now, &io(&audit, &store))
        .expect("staged");
    // The daemon dies mid-send: the image is the file as the post starts.
    let image = dir.path().join("crash-image.json");
    let crash = CrashSender {
        table: store.path.clone(),
        image: image.clone(),
    };
    t.decide(
        id2,
        &owner(&cfg),
        &approve(),
        now,
        &crash,
        &io(&audit, &store),
    )
    .expect("sent");
    let mut back = load_table(&image).expect("loads");
    assert_eq!(
        back.get(id2).map(|s| &s.state),
        Some(&SendState::Unknown(RESTART_DURING_SEND.into()))
    );
    assert_eq!(
        back.get(id).map(|s| &s.state),
        Some(&SendState::Unknown("timeout".into()))
    );
    let sender = Sender::new(Ok(()));
    for i in [id, id2] {
        assert_eq!(
            back.decide(
                i,
                &owner(&cfg),
                &approve(),
                now,
                &sender,
                &io(&audit, &store)
            ),
            Err(SendError::AlreadyDecided)
        );
    }
    assert!(sender.sent.borrow().is_empty());
}

/// The persisted file never holds a `Sending` record after a restore, and a
/// fresh home loads an empty table.
#[test]
fn table_round_trips_and_restores_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = table_path(dir.path());
    assert!(load_table(&path).expect("empty").get(1).is_none());
    let (_, t, id, _) = staged();
    JsonSendStore { path: path.clone() }
        .save(&t)
        .expect("saved");
    let back = load_table(&path).expect("loads");
    assert_eq!(back.get(id).map(|s| s.text()), Some(TEXT));
    // #10423: a Staged record never comes back approvable.
    let mut sending = t.clone().restored();
    assert_eq!(
        sending.get(id).map(|s| &s.state),
        Some(&SendState::NotSent(RESTART_WHILE_STAGED.into()))
    );
    sending = serde_json::from_str::<StagedSends>(
        &serde_json::to_string(&t)
            .expect("enc")
            .replace("\"staged\"", "\"sending\""),
    )
    .expect("dec")
    .restored();
    assert_eq!(
        sending.get(id).map(|s| &s.state),
        Some(&SendState::Unknown(RESTART_DURING_SEND.into()))
    );
}

/// #10423 Fail-Open regression: a table that cannot be read is an error,
/// never an empty table.
#[test]
fn corrupt_table_refuses_to_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = table_path(dir.path());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, "{ not json").expect("write");
    let err = load_table(&path).expect_err("refused");
    assert!(err.0.contains("parse"), "{err}");
}

/// #10423: the audit sink holds no text, and a failed write is an `Err`.
#[test]
fn audit_sink_lines_hold_no_text_and_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let audit = JsonlSendAudit {
        path: send_audit_path(dir.path()),
    };
    let (cfg, mut t, _, now) = staged();
    let store = JsonSendStore {
        path: table_path(dir.path()),
    };
    let id = t
        .stage(
            &cfg,
            "bob-duetto",
            None,
            TEXT,
            now,
            &SendIo {
                audit: &audit,
                store: &store,
            },
        )
        .expect("staged");
    let body = std::fs::read_to_string(&audit.path).expect("read");
    assert!(body.contains(&format!("\"id\":{id}")), "{body}");
    assert!(!body.contains(TEXT), "{body}");
    let blocked = JsonlSendAudit {
        path: dir.path().join("file-not-dir").join("a.jsonl"),
    };
    std::fs::write(dir.path().join("file-not-dir"), "x").expect("write");
    assert!(
        blocked
            .record(AuditLine {
                id: None,
                event: "staged",
                org: None,
                code: "x".into()
            })
            .is_err()
    );
}

fn memory_resolver(raw: &str, value: &str) -> StoreResolver {
    let dir = tempfile::tempdir().expect("tempdir").keep();
    let backend = Arc::new(MemoryBackend::new());
    let index = NamesIndex::at(dir);
    let parsed = SecretRef::parse(raw).expect("ref");
    SecretStore::new(backend.clone(), index.clone())
        .set(
            &parsed.pinned_vault().expect("pinned"),
            parsed.key(),
            &SecretValue::new(value),
        )
        .expect("set");
    StoreResolver::with(backend, index)
}

/// The org scope the resolver tests run under: project `duettoresearch/mpm`.
const SCOPE: Option<&str> = Some("duettoresearch/mpm");
const REF: &str = "secret://duettoresearch/SLACK_BOT";

/// #10423: DOC-74 §15.3 — the owner-pinned, project-pinned and unpinned
/// forms all resolve inside the org's lookup order.
#[test]
fn resolver_reads_pinned_and_unpinned_refs() {
    let r = memory_resolver(REF, "tok-value");
    let owner = r.resolve(SCOPE, REF).expect("owner-pinned");
    assert_eq!(owner.expose(), "tok-value");
    let unpinned = r.resolve(SCOPE, "secret://SLACK_BOT").expect("unpinned");
    assert_eq!(unpinned.expose(), "tok-value");
    let project = memory_resolver("secret://duettoresearch/mpm/APP", "app-value");
    let token = project
        .resolve(SCOPE, "secret://duettoresearch/mpm/APP")
        .expect("project-pinned");
    assert_eq!(token.expose(), "app-value");
    assert_eq!(
        project
            .resolve(SCOPE, "secret://APP")
            .expect("unpinned")
            .expose(),
        "app-value"
    );
}

/// #10423 Fail-Open regression: a ref naming a vault outside the org's
/// lookup order is refused before the store is read, even when that vault
/// holds the key. Failed before the fix, which scoped to the pinned vault.
#[test]
fn resolver_refuses_out_of_scope_and_invalid_refs() {
    let foreign = memory_resolver("secret://victim/prod/DB_URL", "victim-value");
    for raw in ["secret://victim/prod/DB_URL", "secret://victim/DB_URL"] {
        assert_eq!(
            foreign.resolve(SCOPE, raw).map(|_| ()),
            Err("vault_out_of_scope".into()),
            "{raw}"
        );
    }
    let sibling = memory_resolver("secret://duettoresearch/other/KEY", "sib-value");
    assert_eq!(
        sibling
            .resolve(SCOPE, "secret://duettoresearch/other/KEY")
            .map(|_| ()),
        Err("vault_out_of_scope".into())
    );
    let r = memory_resolver(REF, "tok-value");
    assert_eq!(
        r.resolve(None, REF).map(|_| ()),
        Err("no_secret_scope".into())
    );
    assert_eq!(
        r.resolve(SCOPE, "xoxb-literal").map(|_| ()),
        Err("invalid_ref".into())
    );
    let err = r
        .resolve(SCOPE, "secret://duettoresearch/OTHER")
        .map(|_| ())
        .expect_err("missing");
    assert!(!err.contains("tok-value"), "{err}");
}

/// #10423: the scope table — every DOC-74 §15.3 form, under both scope shapes.
#[test]
fn ref_scope_accepts_spec_forms_and_refuses_foreign_vaults() {
    let cases: &[(Option<&str>, &str, Result<(), &str>)] = &[
        (SCOPE, "secret://KEY", Ok(())),
        (SCOPE, "secret://duettoresearch/KEY", Ok(())),
        (SCOPE, "secret://duettoresearch/mpm/KEY", Ok(())),
        (SCOPE, "secret://DuettoResearch/MPM/KEY", Ok(())),
        (
            SCOPE,
            "secret://duettoresearch/other/KEY",
            Err("vault_out_of_scope"),
        ),
        (SCOPE, "secret://victim/KEY", Err("vault_out_of_scope")),
        (SCOPE, "secret://victim/prod/KEY", Err("vault_out_of_scope")),
        (Some("duettoresearch"), "secret://KEY", Ok(())),
        (
            Some("duettoresearch"),
            "secret://duettoresearch/KEY",
            Ok(()),
        ),
        (
            Some("duettoresearch"),
            "secret://duettoresearch/mpm/KEY",
            Err("vault_out_of_scope"),
        ),
        (None, "secret://duettoresearch/KEY", Err("no_secret_scope")),
        (Some("a/b/c"), "secret://KEY", Err("invalid_scope")),
        (Some(""), "secret://KEY", Err("invalid_scope")),
        (SCOPE, "secret://", Err("invalid_ref")),
        (SCOPE, "xoxb-literal", Err("invalid_ref")),
    ];
    for (scope, raw, want) in cases {
        let got = parse_in_scope(*scope, raw).map(|_| ());
        assert_eq!(&got, want, "{scope:?} {raw}");
    }
}

#[test]
fn health_reports_present_missing_and_invalid() {
    let r = memory_resolver(REF, "tok-value");
    assert_eq!(r.health(SCOPE, REF), RefHealth::Present);
    assert_eq!(r.health(SCOPE, "secret://SLACK_BOT"), RefHealth::Present);
    assert_eq!(
        r.health(SCOPE, "secret://duettoresearch/OTHER"),
        RefHealth::Missing
    );
    assert_eq!(
        r.health(SCOPE, "secret://victim/SLACK_BOT"),
        RefHealth::Invalid("vault_out_of_scope")
    );
}

struct Poster(Mutex<Vec<(String, String)>>, Result<(), SendFailure>);
impl SlackPost for Poster {
    fn post(&self, _t: &Secret<String>, channel: &str, text: &str) -> Result<Posted, SendFailure> {
        self.0
            .lock()
            .expect("lock")
            .push((channel.to_owned(), text.to_owned()));
        self.1.clone().map(|()| Posted {
            channel: "D1".into(),
            ts: "1.0".into(),
        })
    }
}

struct Fixed(Result<(), String>);
impl SecretResolver for Fixed {
    fn resolve(&self, _s: Option<&str>, _r: &str) -> Result<Secret<String>, String> {
        self.0.clone().map(|()| Secret::new("tok".to_owned()))
    }
}

/// #10423 AC3: the owner's DM, whichever org the send is for.
#[test]
fn approval_request_goes_to_the_owner_dm_for_every_org() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let mut t = StagedSends::default();
    let (a, s) = (Audit::default(), super::staged_send_tests::Store::default());
    for user in ["bob-duetto", "owner@example.com"] {
        let id = t
            .stage(&cfg, user, None, TEXT, 1, &io(&a, &s))
            .expect("staged");
        let poster = Poster(Mutex::new(Vec::new()), Ok(()));
        let send = t.get(id).expect("send");
        let via = request_approval(&cfg, id, send, &poster, &Fixed(Ok(())));
        assert_eq!(via, ApprovalVia::OwnerDm);
        let posted = poster.0.lock().expect("lock");
        assert_eq!(posted[0].0, "U0OWNER01");
        assert!(posted[0].1.contains(TEXT) && posted[0].1.contains(&send.digest));
    }
}

#[test]
fn approval_falls_back_to_the_dashboard() {
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let (_, t, id, _) = staged();
    let send = t.get(id).expect("send");
    let ok = Poster(Mutex::new(Vec::new()), Ok(()));
    let via = request_approval(&cfg, id, send, &ok, &Fixed(Err("SECRET".into())));
    assert_eq!(via, ApprovalVia::Dashboard("credential_unavailable".into()));
    assert!(ok.0.lock().expect("lock").is_empty());
    let mut no_owner = cfg.clone();
    no_owner.owner = None;
    let via = request_approval(&no_owner, id, send, &ok, &Fixed(Ok(())));
    assert_eq!(via, ApprovalVia::Dashboard("no_owner".into()));
    let rejected = Poster(
        Mutex::new(Vec::new()),
        Err(SendFailure::Rejected("not_in_channel".into())),
    );
    let via = request_approval(&cfg, id, send, &rejected, &Fixed(Ok(())));
    assert_eq!(via, ApprovalVia::Dashboard("not_in_channel".into()));
}

/// A file store that refuses every save past the first `max`.
struct FailingFile {
    inner: JsonSendStore,
    saved: std::cell::Cell<usize>,
    max: usize,
}
impl SendStore for FailingFile {
    fn save(&self, t: &StagedSends) -> Result<(), WriteFailed> {
        if self.saved.get() >= self.max {
            return Err(WriteFailed("read-only".into()));
        }
        self.saved.set(self.saved.get() + 1);
        self.inner.save(t)
    }
}

/// #10423 Fail-Open regression: a Deny whose save fails is an error, and the
/// record read back after a restart can never be approved.
#[test]
fn unpersisted_deny_is_an_error_and_never_approvable_after_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = table_path(dir.path());
    let store = FailingFile {
        inner: JsonSendStore { path: path.clone() },
        saved: std::cell::Cell::new(0),
        max: 1,
    };
    let cfg = OrgsConfig::parse(INITIAL).expect("parses");
    let audit = Audit::default();
    let mut t = StagedSends::default();
    let now = 1_000;
    let id = t
        .stage(&cfg, "bob-duetto", None, TEXT, now, &io(&audit, &store))
        .expect("staged");
    let sender = Sender::new(Ok(()));
    let denied = t.decide(
        id,
        &owner(&cfg),
        &Decision::Deny,
        now,
        &sender,
        &io(&audit, &store),
    );
    // The daemon restarts: the file is all that survives.
    let mut back = load_table(&path).expect("loads");
    let ok = JsonSendStore { path };
    let again = back.decide(id, &owner(&cfg), &approve(), now, &sender, &io(&audit, &ok));
    assert_eq!(again, Err(SendError::AlreadyDecided));
    assert!(sender.sent.borrow().is_empty(), "a denied send never posts");
    assert!(matches!(denied, Err(SendError::Persist(_))), "{denied:?}");
    assert_eq!(t.get(id).map(|s| &s.state), Some(&SendState::Denied));
}
