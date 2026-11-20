//! Audit-line parity against `tests/fixtures/audit.jsonl` (Python 0.7.0).

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use common::{read, state_dir};
use trusty_browser::audit::{self, Audit, AuditError, Clock, REASONS};
use trusty_browser::pyjson::Json;

/// A clock the test sets before each line.
#[derive(Clone, Default)]
struct Manual(Arc<Mutex<f64>>);

impl Clock for Manual {
    fn now(&self) -> f64 {
        self.0.lock().map(|t| *t).unwrap_or(0.0)
    }
}

#[test]
fn audit_lines_replay_byte_identically() {
    let state = state_dir();
    let clock = Manual::default();
    let log = Audit::with_clock(state.path(), Box::new(clock.clone()));
    let fixture = read("audit.jsonl");
    for line in fixture.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(Json::Object(fields)) = Json::parse(line) else {
            panic!("line")
        };
        let Some((_, Json::Float(ts))) = fields.first() else {
            panic!("ts first")
        };
        *clock.0.lock().unwrap_or_else(|e| e.into_inner()) = *ts;
        let event = fields[1].1.as_str().unwrap_or_default();
        let rest: Vec<(&str, Option<Json>)> = fields[2..]
            .iter()
            .map(|(k, v)| (k.as_str(), Some(v.clone())))
            .collect();
        if event == "refused" {
            let get = |k: &str| {
                fields
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.as_str())
            };
            log.refuse(
                get("site").unwrap_or_default(),
                get("reason").unwrap_or_default(),
                get("profile"),
                get("email_domain"),
            );
        } else {
            log.write(event, &rest)
                .unwrap_or_else(|e| panic!("{event}: {e}"));
        }
    }
    let written = fs::read(log.path()).unwrap_or_default();
    assert_eq!(
        String::from_utf8_lossy(&written),
        String::from_utf8_lossy(&fixture)
    );
    assert_eq!(written, fixture);
    let mode = fs::metadata(log.path())
        .map(|m| m.permissions().mode() & 0o777)
        .ok();
    assert_eq!(mode, Some(0o600));
}

#[test]
fn audit_drops_none_and_refuses_unknown_fields() {
    let state = state_dir();
    let log = Audit::with_clock(state.path(), Box::new(Manual::default()));
    let line = log.line("revoke", &[("grant", None), ("tab", Some(Json::Int(3)))]);
    assert_eq!(
        line.ok().as_deref(),
        Some(r#"{"ts":0.0,"event":"revoke","tab":3}"#)
    );
    assert!(matches!(
        log.write("x", &[("email", Some(Json::from("a")))]),
        Err(AuditError::UnknownField)
    ));
}

#[test]
fn audit_refuses_a_shared_log() {
    let state = state_dir();
    let log = Audit::new(state.path());
    fs::write(log.path(), b"").unwrap_or_default();
    fs::set_permissions(log.path(), fs::Permissions::from_mode(0o644)).unwrap_or_default();
    assert!(matches!(
        log.write("revoke", &[]),
        Err(AuditError::NotPrivate)
    ));
}

#[test]
fn audit_rotates_like_python() {
    let state = state_dir();
    let log = Audit::new(state.path());
    let big = vec![b'x'; audit::ROTATE_BYTES as usize];
    let private = |p: &std::path::Path, body: &[u8]| {
        fs::write(p, body).unwrap_or_default();
        fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap_or_default();
    };
    private(log.path(), &big);
    private(&log.generation(1), b"one\n");
    private(&log.generation(2), b"two\n");
    log.write("revoke", &[]).unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<String> = ["audit.jsonl.1", "audit.jsonl.2"]
        .iter()
        .map(|n| state.path().join(n))
        .map(|p| {
            fs::metadata(p)
                .map(|m| m.len().to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        names,
        [audit::ROTATE_BYTES.to_string(), "4".to_owned()],
        ".1 = old live, .2 = old .1"
    );
    assert!(!state.path().join("audit.jsonl.3").exists(), "keep 2");
    let live = fs::read_to_string(log.path()).unwrap_or_default();
    assert!(
        live.starts_with(r#"{"ts":"#) && live.ends_with("\"event\":\"revoke\"}\n"),
        "{live}"
    );
}

#[test]
fn email_domain_matches_python() {
    assert_eq!(
        audit::email_domain("bêta.ünï@exämple.test").as_deref(),
        Some("exämple.test")
    );
    assert_eq!(
        audit::email_domain("a@b@EXAMPLE.Test ").as_deref(),
        Some("example.test")
    );
    assert_eq!(
        audit::email_domain("a@Straße.de").as_deref(),
        Some("strasse.de")
    );
    assert_eq!(audit::email_domain("no-at"), None);
    assert_eq!(audit::email_domain("a@ "), None);
    assert_eq!(audit::email_domain(&format!("a@{}", "d".repeat(254))), None);
}

#[test]
fn refusals_use_known_reasons() {
    let fixture = String::from_utf8(read("audit.jsonl")).unwrap_or_default();
    for line in fixture
        .lines()
        .filter(|l| l.contains("\"event\":\"refused\""))
    {
        let v = Json::parse(line.as_bytes()).unwrap_or(Json::Null);
        let reason = v.get("reason").and_then(Json::as_str).unwrap_or_default();
        assert!(REASONS.contains(&reason), "{reason}");
    }
}
