//! SPEC-OKF-01 §6.2 criteria 85, 129, 135 and 141, and the CLI `feed` verb
//! (criterion 51), for slice S8 of #10746 (feature `search-feed`).
//!
//! Why: an import is saved before it is searchable. A failed or
//! unconfirmed feed must leave every touched concept Pending, the report
//! must say "not fully searchable", and the CLI must exit 4. The index must
//! never hold `_sources/` or `_state/`, and a full reindex needs a scope
//! proof. A tombstone trusty-search cannot detect must never read
//! Withdrawn.
//! What: imports through the library and through `cli::run_with_feed`,
//! against the scripted fake daemon in `s8_support/fake.rs`, with and
//! without D1; one run of the built binary against an absent socket. The
//! live-daemon half of criterion 141 runs out of tree.
//! Test: `cargo test -p trusty-okf --features search-feed --test
//! import_feed_criteria`.

#[path = "s8_support/fake.rs"]
mod fake;
mod s8_support;

use std::ffi::OsString;

use fake::{Fake, INDEX, ident, removed_paths};
use s8_support::{Fx, frontmatter, id, instant, now};
use serde_json::json;
use trusty_okf::OkfError;
use trusty_okf::build::ledger::Ledger;
use trusty_okf::build::rebuild::rebuild;
use trusty_okf::bundle::{Bundle, VerificationState};
use trusty_okf::feed::{FeedStatus, ReindexDecision, feed, reindex_after_rebuild};
use trusty_okf::format::{
    Actor, Concept, Verification, WriteMode, content_hash, is_detectable_tombstone,
};
use trusty_okf::profile::Profile;
use trusty_okf::state::{IndexState, IndexStateDir, LiveIdentity};

fn state(fx: &Fx) -> IndexStateDir {
    IndexStateDir::new(fx.tmp.path().join("machine"))
}

fn run_cli(fx: &Fx, sink: &Fake, extra: &[&str]) -> (i32, String) {
    let st = fx.tmp.path().join("machine");
    let mut args: Vec<String> = vec![
        "import".into(),
        "run".into(),
        fx.root.to_string_lossy().into_owned(),
        "--from".into(),
        format!("dir:{}", fx.src.display()),
        "--feed".into(),
        "--index".into(),
        INDEX.into(),
        "--state-dir".into(),
        st.to_string_lossy().into_owned(),
        "--socket".into(),
        "/nonexistent/fake.sock".into(),
        "--now".into(),
        "2026-11-04T12:00:00Z".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = trusty_okf::cli::run_with_feed(&args, &mut out, &mut err, sink);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    (code, text)
}

/// Why: criterion 85 — with the feed failing, the report gives
/// `written > 0`, `searchable = 0`, `pending > 0`, says "not fully
/// searchable", and the CLI exits 4.
/// What: `import run --feed` against a fake daemon that is down.
/// Test: self-contained.
#[test]
fn c85_failed_feed_reports_not_fully_searchable_and_exits_4() {
    let fx = Fx::new(&[("a.md", "Alpha one two.\n"), ("b.md", "Beta three.\n")]);
    let sink = Fake::new(&fx.root, true);
    sink.script(|s| s.down = true);
    let (code, out) = run_cli(&fx, &sink, &["--json"]);
    assert_eq!(code, 4, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["written"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["searchable"], 0);
    assert!(v["pending"].as_u64().unwrap() > 0);
    assert_eq!(v["fed"], true);
    assert_eq!(v["fully_searchable"], false);
    fx.write("c.md", "Gamma.\n");
    let (code, out) = run_cli(&fx, &sink, &[]);
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains("summary: complete, not fully searchable; exit 4"),
        "{out}"
    );
}

/// Why: §4.10 (#10746 S8 fix) — once the import has written, a failure in
/// the feed step is a run-level error after writes began: the report is
/// printed and the CLI exits 1, never 2.
/// What: the fake corrupts the concept file during its put, so reading the
/// index states after the pass fails with a parse error.
/// Test: self-contained.
#[test]
fn cli_post_write_feed_error_prints_the_report_and_exits_1() {
    let fx = Fx::new(&[("a.md", "Alpha one two.\n")]);
    let sink = Fake::new(&fx.root, true);
    let root = fx.root.clone();
    sink.script(|s| {
        s.on_put = Some(Box::new(move |wire: &str| {
            std::fs::write(root.join(wire), "---\nkey: [unclosed\n---\nbody\n").unwrap();
        }))
    });
    let (code, out) = run_cli(&fx, &sink, &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("summary: complete"),
        "no report printed: {out}"
    );
    assert!(out.contains("feed pass failed after the import"), "{out}");
}

/// Why: criterion 85 through the built binary: a socket with no daemon is a
/// failed feed (F1), and the process exits 4 after saving the import.
/// What: the binary with `--feed` and an absent socket.
/// Test: self-contained.
#[test]
fn cli_binary_exits_4_when_the_feed_cannot_reach_a_daemon() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-okf"))
        .arg("import")
        .arg("run")
        .arg(&fx.root)
        .arg("--from")
        .arg(format!("dir:{}", fx.src.display()))
        .args(["--feed", "--index", INDEX, "--state-dir"])
        .arg(fx.tmp.path().join("machine"))
        .arg("--socket")
        .arg(fx.tmp.path().join("absent.sock"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(text.contains("not fully searchable"), "{text}");
    assert!(fx.root.join("a.md").is_file());
}

/// Why: criterion 129 — a chunk under `_sources/` makes the pass remove the
/// prefix, push nothing, append no journal line and return `IndexScope`;
/// after a `rebuild`, an allowlist binding with no exclusion gets no
/// reindex, and the literal exclusion proof gets exactly one.
/// What: an imported bundle, a seeded leak, then `rebuild` and the reindex
/// gate under two `config.get` replies.
/// Test: self-contained.
#[tokio::test]
async fn c129_scope_leak_refuses_and_reindex_needs_an_exclusion_proof() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    fx.import();
    let sink = Fake::new(&fx.root, true);
    sink.seed("_sources/registry.toml", "leaked row");
    let st = state(&fx);
    let b = Bundle::open(&fx.root).unwrap().0;
    let got = feed(&b, &st, INDEX, &sink).await;
    match got {
        Err(OkfError::IndexScope { leaked, .. }) => {
            assert_eq!(
                leaked,
                vec![std::path::PathBuf::from("_sources/registry.toml")]
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(sink.calls("search.index.files.remove").len(), 1);
    assert!(sink.calls("search.index.file.put").is_empty());
    assert!(
        !st.path().exists()
            || walkdir::WalkDir::new(st.path())
                .into_iter()
                .all(|e| !e.unwrap().file_type().is_file())
    );

    let ledger = Ledger::load(&fx.root, s8_support::STORE).unwrap();
    assert_eq!(ledger.records().count(), 1);
    rebuild(&fx.root, &Profile::default_profile(), &[], now()).unwrap();
    sink.script(|s| {
        s.config = Some(
            json!({"extensions": ["md"], "exclude_globs": [], "extra_skip_dirs": [],
                               "invalid_exclude_globs": []}),
        )
    });
    let d = reindex_after_rebuild(&sink, INDEX).await.unwrap();
    assert!(matches!(d, ReindexDecision::NotProven(_)), "{d:?}");
    assert!(sink.calls("search.index.reindex").is_empty());
    sink.script(|s| {
        s.config = Some(json!({"exclude_globs": ["_sources/**", "_state/**"],
                               "extra_skip_dirs": [], "invalid_exclude_globs": []}))
    });
    assert_eq!(
        reindex_after_rebuild(&sink, INDEX).await.unwrap(),
        ReindexDecision::Sent
    );
    assert_eq!(sink.calls("search.index.reindex").len(), 1);
    sink.assert_no_bookkeeping_puts();
}

/// Why: criterion 135 — with D1, an `Unreadable` live identity reads every
/// concept Pending and the old identity reads Searchable again; without D1
/// every put is acknowledged and every probe passes, yet the pass is
/// `Unconfirmed`, nothing reads Searchable and `import run --feed` exits 4,
/// and a second pass pushes nothing; a D1 put reply with another hash fails
/// the task and appends no journal line.
/// What: three fakes over imported bundles.
/// Test: self-contained.
#[test]
fn c135_identity_and_no_d1_paths_never_read_searchable() {
    // The CLI runs its own runtime, so this test drives the async calls
    // on one it owns, outside any runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fx = Fx::new(&[("a.md", "Alpha one.\n"), ("b.md", "Beta two.\n")]);
    fx.import();
    let sink = Fake::new(&fx.root, true);
    let st = state(&fx);
    let b = Bundle::open(&fx.root).unwrap().0;
    let r = rt.block_on(feed(&b, &st, INDEX, &sink)).unwrap();
    assert_eq!(r.status, FeedStatus::Searchable, "{r:?}");
    let old = LiveIdentity::Known(ident("inst-1", 1));
    for c in ["a", "b"] {
        let down = b
            .revision_state(&id(c), &st, INDEX, &LiveIdentity::Unreadable("down".into()))
            .unwrap();
        assert!(matches!(down.index, IndexState::Pending { .. }), "{down:?}");
        assert_eq!(
            b.revision_state(&id(c), &st, INDEX, &old).unwrap().index,
            IndexState::Searchable
        );
    }
    sink.assert_no_bookkeeping_puts();

    let fx2 = Fx::new(&[("a.md", "Alpha one.\n"), ("b.md", "Beta two.\n")]);
    let no_d1 = Fake::new(&fx2.root, false);
    let (code, out) = run_cli(&fx2, &no_d1, &["--json"]);
    assert_eq!(code, 4, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["searchable"], 0, "{v}");
    let puts = no_d1.calls("search.index.file.put").len();
    assert_eq!(puts, 2);
    let st2 = state(&fx2);
    let b2 = Bundle::open(&fx2.root).unwrap().0;
    let summary = b2
        .state_summary(&st2, INDEX, &LiveIdentity::Unsupported)
        .unwrap();
    assert_eq!(summary.searchable, 0, "{summary:?}");
    let again = rt.block_on(feed(&b2, &st2, INDEX, &no_d1)).unwrap();
    assert_eq!(again.status, FeedStatus::Unconfirmed);
    assert_eq!(
        no_d1.calls("search.index.file.put").len(),
        puts,
        "a second pass pushed"
    );
    no_d1.assert_no_bookkeeping_puts();

    let fx3 = Fx::new(&[("a.md", "Alpha.\n")]);
    fx3.import();
    let liar = Fake::new(&fx3.root, true);
    liar.script(|s| s.wrong_hash = true);
    let st3 = state(&fx3);
    let r = rt
        .block_on(feed(
            &Bundle::open(&fx3.root).unwrap().0,
            &st3,
            INDEX,
            &liar,
        ))
        .unwrap();
    assert_ne!(r.status, FeedStatus::Searchable);
    assert_eq!(r.acknowledged, 0, "{r:?}");
    let journal = trusty_okf::state::IndexJournal::open(&st3, &fx3.root, INDEX).unwrap();
    // #10876: the pass journals the identity it read; nothing else.
    assert!(
        journal
            .read()
            .unwrap()
            .iter()
            .all(|r| matches!(r, trusty_okf::state::IndexRecord::Observed { .. })),
        "a mismatched put was journaled"
    );
}

/// Why: criterion 141 — 2,000 invalidated events stay out of frontmatter
/// (at most 10 history events there) and `revision_state` counts them; the
/// tombstone is detectable and withdraws; a tombstone whose frontmatter an
/// out-of-band 70 KiB key pushed past 64 KiB still lands, with a
/// `tombstone_undetectable` error finding, and reads WithdrawPending after
/// every feed pass, never Withdrawn.
/// What: verify-and-edit rounds on an imported concept, then source
/// removal; a second imported concept hand-edited, then removed; two feed
/// passes against a D1 fake.
/// Test: self-contained.
#[tokio::test]
async fn c141_overgrown_tombstone_lands_undetectable_and_never_withdrawn() {
    let fx = Fx::new(&[("long.md", "Long lived.\n"), ("big.md", "Big.\n")]);
    fx.import();
    let mut b = Bundle::open(&fx.root).unwrap().0;
    let long = id("long");
    let mut t = instant("2026-11-04T00:00:00Z");
    for round in 0..200 {
        let seen = content_hash(&b.get(&long).unwrap());
        for _ in 0..10 {
            t += chrono::Duration::seconds(1);
            let v = Verification {
                by: Actor::Process("rev".into()),
                at: t,
                stale_after: None,
            };
            b.verify(&long, v, &seen).unwrap();
        }
        let mut c: Concept = b.get(&long).unwrap();
        c.body = format!("Long lived, round {round}.\n");
        b.put(c, Actor::Process("ed".into()), WriteMode::Lenient, t)
            .unwrap();
    }
    let fm = frontmatter(&fx.root, "long.md");
    let in_fm = fm
        .get("verified")
        .and_then(|v| v.as_sequence())
        .map_or(0, Vec::len);
    assert!(in_fm <= 10, "{in_fm} events in frontmatter");
    let st = state(&fx);
    let rs = b
        .revision_state(&long, &st, INDEX, &LiveIdentity::Unsupported)
        .unwrap();
    assert_eq!(
        rs.verification,
        VerificationState::Unverified { history: 2000 }
    );

    let big = fx.root.join("big.md");
    let text = std::fs::read_to_string(&big).unwrap();
    let pad = format!("oob_blob: {}\n", "x".repeat(70 * 1024));
    std::fs::write(&big, text.replacen("---\n", &format!("---\n{pad}"), 1)).unwrap();
    fx.remove("long.md");
    fx.remove("big.md");
    let r = fx
        .import_with(
            &fx.spec(),
            &trusty_okf::extract::NoWorker,
            instant("2026-11-05T00:00:00Z"),
        )
        .unwrap();
    assert_eq!(r.tombstoned, 2, "{r:?}");
    let undetectable: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.kind == "tombstone_undetectable")
        .collect();
    assert_eq!(undetectable.len(), 1, "{:?}", r.findings);
    assert_eq!(
        (
            undetectable[0].item_id.as_str(),
            undetectable[0].severity.as_str()
        ),
        ("big.md", "error")
    );
    let long_text = fx.read("long.md");
    assert!(is_detectable_tombstone(&long_text));
    assert!(trusty_common::knowledge_document::is_tombstone(&long_text));
    let big_text = fx.read("big.md");
    assert!(big_text.contains("source_status: deleted") && big_text.contains("oob_blob"));
    assert!(!trusty_common::knowledge_document::is_tombstone(&big_text));

    let sink = Fake::new(&fx.root, true);
    sink.seed("long.md", "Long lived.");
    sink.seed("big.md", "Big.");
    let live = LiveIdentity::Known(ident("inst-1", 1));
    for pass in 0..2 {
        sink.clear_calls();
        let b = Bundle::open(&fx.root).unwrap().0;
        feed(&b, &st, INDEX, &sink).await.unwrap();
        assert!(
            removed_paths(&sink).contains("big.md"),
            "pass {pass}: big.md not removed"
        );
        assert!(sink.chunks_of("long.md").is_empty() && sink.chunks_of("big.md").is_empty());
        let big_state = b
            .revision_state(&id("big"), &st, INDEX, &live)
            .unwrap()
            .index;
        assert_eq!(big_state, IndexState::WithdrawPending, "pass {pass}");
        let long_state = b.revision_state(&long, &st, INDEX, &live).unwrap().index;
        assert_eq!(long_state, IndexState::Withdrawn, "pass {pass}");
    }
}

/// Why: the local §4.8 rule must match trusty-common's `is_tombstone`
/// byte for byte, or trusty-okf would mispredict what the daemon removes.
/// What: a table of documents through both functions.
/// Test: self-contained.
#[test]
fn tombstone_rule_matches_trusty_common() {
    let near = 64 * 1024 - 64;
    let cases = vec![
        "---\nsource_id: s\nsource_status: deleted\n---\nb\n".to_string(),
        "---\nsource_id: ''\nsource_status: deleted\n---\n".to_string(),
        "\u{feff}---\nsource_id: s\nsource_status: deleted\n---\n".to_string(),
        "---\nsource_status: deleted\n---\n".to_string(),
        "---\nsource_id: s\nsource_status: deleted\n".to_string(),
        format!(
            "---\nsource_id: s\nsource_status: deleted\nk: {}\n---\n",
            "a".repeat(near)
        ),
        format!(
            "---\nsource_id: s\nsource_status: deleted\nk: {}\n---\n",
            "a".repeat(64 * 1024)
        ),
        "---\r\nsource_id: s\r\nsource_status: deleted\r\n---\r\n".to_string(),
    ];
    for c in cases {
        assert_eq!(
            is_detectable_tombstone(&c),
            trusty_common::knowledge_document::is_tombstone(&c),
            "{:?}",
            &c[..c.len().min(60)]
        );
    }
}

/// Why: criterion 51 wired to the real CLI — the `feed` verb exits non-zero
/// unless the pass is Searchable, and 2 on a refusal before any push.
/// What: `feed` against a down fake (4), a D1 fake (0), and a root the
/// index does not contain (2).
/// Test: self-contained.
#[test]
fn c51_cli_feed_verb_exits_non_zero_unless_searchable() {
    let fx = Fx::new(&[("a.md", "Alpha one.\n")]);
    fx.import();
    let st = fx.tmp.path().join("machine");
    let args = |extra: &[&str]| -> Vec<OsString> {
        let mut a: Vec<String> = vec![
            "feed".into(),
            fx.root.to_string_lossy().into_owned(),
            "--index".into(),
            INDEX.into(),
            "--state-dir".into(),
            st.to_string_lossy().into_owned(),
            "--socket".into(),
            "/nonexistent".into(),
        ];
        a.extend(extra.iter().map(|s| s.to_string()));
        a.into_iter().map(OsString::from).collect()
    };
    let call = |sink: &Fake| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        trusty_okf::cli::run_with_feed(&args(&[]), &mut out, &mut err, sink)
    };
    let down = Fake::new(&fx.root, true);
    down.script(|s| s.down = true);
    assert_eq!(call(&down), 4);
    assert_eq!(call(&Fake::new(&fx.root, true)), 0);
    assert_eq!(call(&Fake::new(&fx.tmp.path().join("elsewhere"), true)), 2);
}
