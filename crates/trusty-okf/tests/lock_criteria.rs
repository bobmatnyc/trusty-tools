//! The §4.4 bundle lock (#10746 S5): concurrent writers lose no update.
//!
//! Why: without one writer at a time, two writers that each read `log.md`
//! or `_sources/registry.toml`, change it and write it back drop each
//! other's entries or rows.
//! What: ingest runs, `put`s and registrations race on one bundle from
//! several threads, several rounds; every entry and every row survives.
//! Test: `cargo test -p trusty-okf --test lock_criteria`.

use std::path::Path;
use std::sync::{Arc, Barrier};

use chrono::{DateTime, FixedOffset};
use serde_yaml::Mapping;
use trusty_okf::build::ingest::{SourceItem, ingest_items};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceRegistry, SourceSpec};
use trusty_okf::bundle::Bundle;
use trusty_okf::format::{Actor, Concept, ConceptId, WriteMode};
use trusty_okf::profile::Profile;

const N: usize = 25;

/// One writer thread's work: bundle root and a tag.
type Job = fn(&Path, &str);

fn now() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z").unwrap()
}

fn spec(id: &str) -> SourceSpec {
    SourceSpec::new(
        id,
        Some(id),
        Locator::Connector {
            kind: "gmail".into(),
            params: ConnectorParams::new(),
        },
        "2026-11-04T00:00:00Z",
    )
}

fn items(prefix: &str) -> Vec<SourceItem> {
    (0..N)
        .map(|n| SourceItem {
            item_id: format!("{prefix}{n}"),
            fingerprint: format!("fp-{prefix}{n}"),
            name: format!("{prefix} {n}"),
            title: format!("{prefix} {n}"),
            body: format!("body {prefix} {n}"),
            ..SourceItem::default()
        })
        .collect()
}

fn ingest_run(root: &Path, source: &str) {
    let r = ingest_items(
        root,
        &Profile::default_profile(),
        &spec(source),
        &items(source),
        false,
        now(),
    )
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}

fn puts(root: &Path, prefix: &str) {
    for n in 0..N {
        let mut b = Bundle::open(root).unwrap().0;
        let mut m = Mapping::new();
        m.insert("type".into(), "Note".into());
        let id = ConceptId::parse(&format!("{prefix}/c{n}")).unwrap();
        let c = Concept::new(id, m, format!("{prefix} {n}\n"));
        b.put(c, Actor::Process("t".into()), WriteMode::Lenient, now())
            .unwrap();
    }
}

fn registrations(root: &Path, prefix: &str) {
    for n in 0..N {
        SourceRegistry::upsert_at(root, spec(&format!("{prefix}{n}"))).unwrap();
    }
}

/// Why: §4.4 — one writer at a time; concurrent ingest runs, puts and
/// registrations must not lose `log.md` entries or registry rows.
/// What: six threads start together on one bundle, for several rounds:
/// two ingest runs, two put loops, two registration loops. Every round,
/// `log.md` holds one `**Created**` entry per concept written and the
/// registry one row per registration.
/// Test: self-contained.
#[test]
fn lock_prevents_lost_log_and_registry_updates() {
    for round in 0..4 {
        let tmp = tempfile::tempdir().unwrap();
        let root = Arc::new(tmp.path().canonicalize().unwrap());
        let gate = Arc::new(Barrier::new(6));
        let jobs: Vec<(&str, Job)> = vec![
            ("ia", ingest_run),
            ("ib", ingest_run),
            ("pa", puts),
            ("pb", puts),
            ("ra", registrations),
            ("rb", registrations),
        ];
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(tag, job)| {
                let (root, gate) = (root.clone(), gate.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    job(&root, tag);
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread");
        }
        let log = std::fs::read_to_string(root.join("log.md")).unwrap();
        assert_eq!(
            log.matches("**Created**").count(),
            4 * N,
            "round {round}: log.md"
        );
        let reg = SourceRegistry::load(&root).unwrap();
        assert_eq!(reg.sources.len(), 2 * N, "round {round}: registry rows");
    }
}
