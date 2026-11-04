//! SPEC-OKF-01 §6.2 criteria 86-98, 132 and 139 (adapter half), for slice
//! S9 of #10746 (email and Google Docs adapter contract).
//!
//! Why: an authenticated source runs as an adapter process that gets
//! exactly its caller's environment list; trusty-okf owns IDs, provenance,
//! normalisation, the ledger and tombstones, and must never tombstone on a
//! listing it cannot show is whole.
//! What: each test runs a fake adapter process (a shell script fed by
//! fixture files, `s9_support`) through `AdapterSource`, `plan_import` and
//! `import`, or through the `trusty-okf` binary. No live account.
//! Test: `cargo test -p trusty-okf --test adapter_criteria`.

#![cfg(unix)]

mod s9_support;

use std::collections::BTreeMap;
use std::time::Duration;

use s9_support::{FakeAdapter, alive, docs_spec, mail_spec, msg, msg_entry, read, s, spec};
use serde_json::{Value, json};
use trusty_okf::OkfError;
use trusty_okf::bundle::Bundle;
use trusty_okf::extract::NoWorker;
use trusty_okf::import::adapter::{AdapterCommand, AdapterLimits, AdapterSource};
use trusty_okf::import::{
    ContentPolicy, ImportReport, ImportSpec, ItemReport, exit_code, import, plan_import,
};

fn now() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z").unwrap()
}

/// Plan and import `spec` through a fresh adapter process.
fn run(fake: &FakeAdapter, spec: &ImportSpec) -> Result<ImportReport, OkfError> {
    let mut src = AdapterSource::spawn(&fake.cmd(), spec, AdapterLimits::default())?;
    let plan = plan_import(&Bundle::open(&fake.root)?.0, spec, &mut src)?;
    import(
        &mut Bundle::open(&fake.root)?.0,
        &plan,
        &mut src,
        &NoWorker,
        now(),
    )
}

fn item<'a>(r: &'a ImportReport, id: &str) -> &'a ItemReport {
    r.items
        .iter()
        .find(|i| i.item_id == id)
        .unwrap_or_else(|| panic!("{id} not in {:?}", r.items))
}

/// Every file under `root` with its bytes, `_sources/` included.
fn tree(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(Result::unwrap)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (rel, std::fs::read(e.path()).unwrap())
        })
        .collect()
}

fn frontmatter(root: &std::path::Path, rel: &str) -> serde_yaml::Mapping {
    let text = read(root, rel);
    serde_yaml::from_str(text.split("---\n").nth(1).unwrap()).unwrap()
}

fn fm_str(m: &serde_yaml::Mapping, k: &str) -> Option<String> {
    m.get(k).and_then(|v| v.as_str()).map(str::to_string)
}

/// Three pages of two mail entries each, and their records.
fn three_pages(fake: &FakeAdapter) {
    fake.page(
        None,
        json!([msg_entry("m1", "t1"), msg_entry("m2", "t1")]),
        Some("p2"),
        false,
    );
    fake.page(
        Some("p2"),
        json!([msg_entry("m3", "t2"), msg_entry("m4", "t2")]),
        Some("p3"),
        false,
    );
    fake.page(
        Some("p3"),
        json!([msg_entry("m5", "t3"), msg_entry("m6", "t3")]),
        None,
        true,
    );
    for (i, (m, t)) in [
        ("m1", "t1"),
        ("m2", "t1"),
        ("m3", "t2"),
        ("m4", "t2"),
        ("m5", "t3"),
        ("m6", "t3"),
    ]
    .iter()
    .enumerate()
    {
        fake.item(msg(
            m,
            t,
            1_767_225_600_000 + i as u64 * 1000,
            "Hi",
            &format!("Body {m}."),
        ));
    }
}

/// Why: criterion 86 — no account or no selection is refused with
/// `SelectionRequired` before any spawn or write, and maps to exit 2.
/// What: mail with no account, mail with no query, docs with no folder or
/// documents; the API refuses before spawning (no PID file), and the CLI
/// exits 2 with the bundle unchanged.
/// Test: self-contained.
#[test]
fn c86_no_selection_is_refused_before_any_spawn_or_write() {
    let fake = FakeAdapter::new("gmail");
    let before = tree(&fake.root);
    for sp in [
        spec("m", "gmail", &[("query", s("in:sent"))]),
        spec("m", "gmail", &[("account", s("work"))]),
        spec("d", "gdocs", &[("account", s("work"))]),
    ] {
        let got = AdapterSource::spawn(&fake.cmd(), &sp, AdapterLimits::default());
        assert!(
            matches!(got, Err(OkfError::SelectionRequired(_))),
            "{got:?}"
        );
        let b = Bundle::open(&fake.root).unwrap().0;
        struct Never;
        impl trusty_okf::import::ImportSource for Never {
            fn list(&mut self, _: Option<&str>) -> Result<trusty_okf::import::Listing, OkfError> {
                panic!("listed before the selection check")
            }
            fn fetch(&mut self, _: &str) -> Result<Vec<u8>, trusty_okf::extract::ExtractError> {
                panic!("fetched before the selection check")
            }
        }
        let planned = plan_import(&b, &sp, &mut Never).map(|_| ImportReport::default());
        assert!(matches!(planned, Err(OkfError::SelectionRequired(_))));
        assert_eq!(exit_code(&planned), 2);
    }
    assert!(fake.pid().is_none(), "an adapter was spawned");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-okf"))
        .args(["import", "run"])
        .arg(&fake.root)
        .args([
            "--from",
            "adapter:gmail",
            "--adapter",
            "/bin/sh",
            "--select",
            "query=in:sent",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(fake.pid().is_none(), "the CLI spawned an adapter");
    assert_eq!(tree(&fake.root), before);
}

/// Why: criterion 87 — every page is read; a run cut at the ceiling reports
/// the cursor and `pagination_exhausted: false`, and tombstones nothing.
/// What: three pages imported; then a run with a ceiling of three.
/// Test: self-contained.
#[test]
fn c87_three_pages_import_and_a_ceiling_tombstones_nothing() {
    let fake = FakeAdapter::new("gmail");
    three_pages(&fake);
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!((r.listed, r.created), (6, 6), "{r:?}");
    assert!(r.pagination_exhausted);
    let mut sp = mail_spec();
    sp.max_items = 3;
    sp.store.tombstone_deleted = true;
    let r = run(&fake, &sp).unwrap();
    assert!(!r.pagination_exhausted && !r.deletion_safe, "{r:?}");
    assert_eq!(
        r.cursor.as_deref(),
        Some("p2"),
        "the cut page is listed again"
    );
    assert_eq!((r.tombstoned, r.missing), (0, 3), "{r:?}");
    for (rel, bytes) in tree(&fake.root) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("source_status: deleted"), "{rel} tombstoned");
    }
}

/// Why: criterion 88 — IDs come from message, thread and file IDs, so two
/// same-day messages with one subject are two concepts.
/// What: two messages, same subject and date, different IDs; one doc.
/// Test: self-contained.
#[test]
fn c88_ids_derive_from_source_ids_not_subjects() {
    let fake = FakeAdapter::new("gmail");
    fake.page(
        None,
        json!([msg_entry("a1", "t9"), msg_entry("a2", "t9")]),
        None,
        true,
    );
    fake.item(msg("a1", "t9", 1_767_225_600_000, "Status", "One."));
    fake.item(msg("a2", "t9", 1_767_225_600_000, "Status", "Two."));
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!(r.created, 2, "{r:?}");
    assert!(read(&fake.root, "mail/mail-src/t9/a1.md").contains("One."));
    assert!(read(&fake.root, "mail/mail-src/t9/a2.md").contains("Two."));

    let docs = FakeAdapter::new("gdocs");
    docs.page(
        None,
        json!([{"item_id": "F-1", "revision": "7"}]),
        None,
        true,
    );
    docs.item(json!({"item_id": "F-1", "revision": "7", "title": "Plan",
        "parts": [{"mime": "text/markdown", "data": "# Plan\n"}]}));
    let r = run(&docs, &docs_spec(true)).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert!(read(&docs.root, "docs/docs-src/F-1.md").contains("# Plan"));
}

/// Why: criterion 89 — each message keeps `thread_id`, `internalDate` and
/// the `Date` header; a thread shares a directory, and the thread concept
/// links its messages in date order.
/// What: a thread listed newest first; then the frontmatter and the thread
/// concept body.
/// Test: self-contained.
#[test]
fn c89_mail_keeps_thread_and_dates_and_links_the_thread() {
    let fake = FakeAdapter::new("gmail");
    fake.page(
        None,
        json!([msg_entry("late", "t1"), msg_entry("early", "t1")]),
        None,
        true,
    );
    fake.item(msg("late", "t1", 1_767_225_660_000, "Re: x", "Later."));
    fake.item(msg("early", "t1", 1_767_225_600_000, "x", "Earlier."));
    run(&fake, &mail_spec()).unwrap();
    let fm = frontmatter(&fake.root, "mail/mail-src/t1/early.md");
    assert_eq!(fm_str(&fm, "thread_id").as_deref(), Some("t1"));
    assert_eq!(
        fm_str(&fm, "internal_date").as_deref(),
        Some("1767225600000")
    );
    assert_eq!(
        fm_str(&fm, "date_header").as_deref(),
        Some("Thu, 1 Jan 2026 00:00:00 +0000")
    );
    assert_eq!(
        fm_str(&fm, "source_timestamp").as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    let thread = read(&fake.root, "mail/mail-src/t1.md");
    let (e, l) = (
        thread
            .find("(/mail/mail-src/t1/early.md)")
            .expect("early linked"),
        thread
            .find("(/mail/mail-src/t1/late.md)")
            .expect("late linked"),
    );
    assert!(e < l, "not in date order:\n{thread}");
}

/// Why: criterion 90 — an unchanged mailbox writes zero bytes; a widened
/// window fetches only the new message.
/// What: two runs over the same listing (no fetch, same tree); then a page
/// with one more message.
/// Test: self-contained.
#[test]
fn c90_unchanged_mailbox_writes_nothing_and_a_wider_window_fetches_only_new() {
    let fake = FakeAdapter::new("gmail");
    three_pages(&fake);
    run(&fake, &mail_spec()).unwrap();
    let before = tree(&fake.root);
    fake.clear_requests();
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!((r.written, r.unchanged), (0, 6), "{r:?}");
    assert!(fake.fetched().is_empty(), "{:?}", fake.fetched());
    assert_eq!(tree(&fake.root), before, "a repeat import wrote bytes");

    fake.page(
        Some("p3"),
        json!([
            msg_entry("m5", "t3"),
            msg_entry("m6", "t3"),
            msg_entry("m0", "t3")
        ]),
        None,
        true,
    );
    fake.item(msg("m0", "t3", 1_767_000_000_000, "Old", "Older."));
    fake.clear_requests();
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert_eq!(fake.fetched(), vec!["m0".to_string()]);
}

/// Why: criterion 91 — a Markdown or HTML export keeps headings, lists,
/// tables and links; a base64 export with no extractor is
/// `unsupported_format`.
/// What: three docs, one per export form.
/// Test: self-contained.
#[test]
fn c91_docs_exports_keep_structure_and_base64_is_unsupported() {
    let fake = FakeAdapter::new("gdocs");
    let md = "# Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n[site](https://example.com)\n";
    let html = "<h1>Title</h1><ul><li>one</li></ul><table><tr><td>1</td></tr></table><a href=\"https://example.com\">site</a>";
    fake.page(
        None,
        json!([{"item_id": "md", "revision": "1"}, {"item_id": "html", "revision": "1"},
        {"item_id": "bin", "revision": "1"}]),
        None,
        true,
    );
    fake.item(
        json!({"item_id": "md", "revision": "1", "parts": [{"mime": "text/markdown", "data": md}]}),
    );
    fake.item(
        json!({"item_id": "html", "revision": "1", "parts": [{"mime": "text/html", "data": html}]}),
    );
    fake.item(json!({"item_id": "bin", "revision": "1",
        "parts": [{"mime": "application/pdf", "encoding": "base64", "data": "JVBERi0="}]}));
    let r = run(&fake, &docs_spec(true)).unwrap();
    let body = read(&fake.root, "docs/docs-src/md.md");
    for want in [
        "# Title",
        "- one",
        "| a | b |",
        "[site](https://example.com)",
    ] {
        assert!(body.contains(want), "{want} lost:\n{body}");
    }
    let body = read(&fake.root, "docs/docs-src/html.md");
    for want in [
        "<h1>Title</h1>",
        "<li>one</li>",
        "<table>",
        "href=\"https://example.com\"",
    ] {
        assert!(body.contains(want), "{want} lost:\n{body}");
    }
    let bin = item(&r, "bin");
    assert_eq!(
        (bin.read.as_str(), bin.reason.as_deref()),
        ("skipped", Some("unsupported_format"))
    );
    assert!(!fake.root.join("docs/docs-src/bin.md").exists());
}

/// Why: criterion 92 — a message with no decodable text part is
/// `degraded: snippet_only`: no concept by default, the flagged snippet
/// under a policy that allows snippets.
/// What: one base64-only message, imported under each policy.
/// Test: self-contained.
#[test]
fn c92_snippet_only_message_is_degraded() {
    let fake = FakeAdapter::new("gmail");
    fake.page(None, json!([msg_entry("s1", "t1")]), None, true);
    fake.item(
        json!({"item_id": "s1", "group": "t1", "revision": "s1", "snippet": "Short preview",
        "parts": [{"mime": "text/plain", "encoding": "base64", "data": "SGk="}]}),
    );
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!(item(&r, "s1").degraded.as_deref(), Some("snippet_only"));
    assert_eq!((r.degraded, r.written), (1, 0), "{r:?}");
    assert!(!r.complete);
    assert!(!fake.root.join("mail/mail-src/t1/s1.md").exists());

    let mut sp = mail_spec();
    sp.policy = ContentPolicy::AllowSnippet;
    let r = run(&fake, &sp).unwrap();
    assert_eq!((r.degraded, r.created), (1, 1), "{r:?}");
    let fm = frontmatter(&fake.root, "mail/mail-src/t1/s1.md");
    assert_eq!(fm_str(&fm, "degraded").as_deref(), Some("snippet_only"));
    assert!(read(&fake.root, "mail/mail-src/t1/s1.md").contains("Short preview"));
}

/// Why: criterion 93 — the default policy imports no raw MIME and extracts
/// no attachment; attachments are listed by name, type and size; quoted
/// text stays, marked as quoted.
/// What: a record carrying a raw MIME field and an attachment.
/// Test: self-contained.
#[test]
fn c93_no_raw_mime_attachments_listed_quotes_kept() {
    let fake = FakeAdapter::new("gmail");
    fake.page(None, json!([msg_entry("q1", "t1")]), None, true);
    let mut rec = msg(
        "q1",
        "t1",
        1_767_225_600_000,
        "Re: plan",
        "Agreed.\n\n> On Monday you wrote:\n> the plan",
    );
    rec["raw"] = json!("RAW-MIME-MARKER Content-Type: multipart/mixed");
    rec["attachments"] = json!([{"name": "deck.pdf", "mime": "application/pdf", "size": 2048}]);
    fake.item(rec);
    run(&fake, &mail_spec()).unwrap();
    let body = read(&fake.root, "mail/mail-src/t1/q1.md");
    assert!(
        body.contains("> On Monday you wrote:\n> the plan"),
        "{body}"
    );
    assert!(
        body.contains("- `deck.pdf` (application/pdf, 2048 bytes)"),
        "{body}"
    );
    for (rel, bytes) in tree(&fake.root) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains("RAW-MIME-MARKER"),
            "{rel}"
        );
    }
}

/// Why: criterion 94 — a docs item marked `trashed` on a complete recursive
/// listing is tombstoned and reported.
/// What: two docs imported; the second listing marks one deleted.
/// Test: self-contained.
#[test]
fn c94_trashed_item_on_a_complete_recursive_listing_is_tombstoned() {
    let fake = FakeAdapter::new("gdocs");
    fake.page(
        None,
        json!([{"item_id": "d1", "revision": "1"}, {"item_id": "d2", "revision": "1"}]),
        None,
        true,
    );
    for d in ["d1", "d2"] {
        fake.item(json!({"item_id": d, "revision": "1", "parts": [{"mime": "text/markdown", "data": "Doc."}]}));
    }
    run(&fake, &docs_spec(true)).unwrap();
    fake.page(
        None,
        json!([{"item_id": "d1", "revision": "1"},
        {"item_id": "d2", "revision": "1", "deleted": true}]),
        None,
        true,
    );
    let r = run(&fake, &docs_spec(true)).unwrap();
    assert!(r.deletion_safe, "{r:?}");
    assert_eq!(item(&r, "d2").write, "tombstoned");
    assert_eq!(r.tombstoned, 1);
    let fm = frontmatter(&fake.root, "docs/docs-src/d2.md");
    assert_eq!(fm_str(&fm, "source_status").as_deref(), Some("deleted"));
}

/// Why: criterion 95 — an interrupted run resumes from the ledger and
/// re-fetches at most one chunk.
/// What: the adapter dies after two fetches; the re-run fetches only the
/// items the ledger lacks (at most the one in flight was fetched before),
/// and the bundle equals an uninterrupted run's.
/// Test: self-contained.
#[test]
fn c95_interrupted_run_resumes_from_the_ledger() {
    let whole = FakeAdapter::new("gmail");
    three_pages(&whole);
    run(&whole, &mail_spec()).unwrap();

    let fake = FakeAdapter::new("gmail");
    three_pages(&fake);
    fake.flag("die_after", "2");
    let r = run(&fake, &mail_spec()).unwrap();
    assert_eq!(r.created, 2, "{r:?}");
    let first: Vec<String> = fake.fetched();
    fake.unflag("die_after");
    fake.clear_requests();
    run(&fake, &mail_spec()).unwrap();
    let again = fake.fetched();
    let refetched = again.iter().filter(|i| first.contains(i)).count();
    assert!(
        refetched <= 1,
        "re-fetched {refetched}: {first:?} then {again:?}"
    );
    assert_eq!(again.len(), 4, "{again:?}");
    let concepts = |root: &std::path::Path| -> BTreeMap<String, Vec<u8>> {
        tree(root)
            .into_iter()
            .filter(|(k, _)| !k.starts_with("_sources") && k != "log.md")
            .collect()
    };
    assert_eq!(
        concepts(&fake.root).keys().collect::<Vec<_>>(),
        concepts(&whole.root).keys().collect::<Vec<_>>()
    );
}

/// Why: criterion 96 (B6, B10) — the import API takes no credential: no
/// token, secret, password or credential parameter or field.
/// What: scans the public lines of the import source, spec and adapter
/// modules.
/// Test: self-contained.
#[test]
fn c96_import_api_has_no_credential_parameter() {
    let files = [
        include_str!("../src/import/source.rs"),
        include_str!("../src/import/plan.rs"),
        include_str!("../src/import/adapter.rs"),
        include_str!("../src/import/record.rs"),
    ];
    for text in files {
        for line in text.lines().filter(|l| l.trim_start().starts_with("pub")) {
            let l = line.to_ascii_lowercase();
            for word in ["token", "secret", "password", "credential", "oauth"] {
                assert!(!l.contains(word), "credential-shaped API: {line}");
            }
        }
    }
}

/// Why: criterion 97 (3m A) — the CLI imports through an adapter process
/// end to end, and the adapter is reaped on cancel.
/// What: the binary imports two messages and exits 0 with the adapter
/// gone; through the API, `cancel` and a request past its deadline each
/// kill and reap the adapter.
/// Test: self-contained.
#[test]
fn c97_cli_imports_through_an_adapter_process_and_reaps_on_cancel() {
    let fake = FakeAdapter::new("gmail");
    fake.page(
        None,
        json!([msg_entry("c1", "t1"), msg_entry("c2", "t1")]),
        None,
        true,
    );
    fake.item(msg("c1", "t1", 1_767_225_600_000, "a", "One."));
    fake.item(msg("c2", "t1", 1_767_225_700_000, "b", "Two."));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-okf"))
        .args(["import", "run"])
        .arg(&fake.root)
        .args(["--from", "adapter:gmail", "--adapter", "/bin/sh"])
        .arg("--adapter-arg")
        .arg(fake.script())
        .arg("--adapter-arg")
        .arg(&fake.dir)
        .args([
            "--adapter-env",
            "PATH=/usr/bin:/bin",
            "--adapter-env",
            "LC_ALL=C",
        ])
        .args([
            "--select",
            "account=work",
            "--select",
            "query=in:sent",
            "--json",
        ])
        .args(["--now", "2026-11-04T12:00:00Z"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["created"], 2, "{report}");
    assert!(read(&fake.root, "mail/import-gmail/t1/c1.md").contains("One."));
    assert!(!alive(fake.pid().unwrap()), "the adapter outlived the CLI");

    let mut src =
        AdapterSource::spawn(&fake.cmd(), &mail_spec(), AdapterLimits::default()).unwrap();
    let pid = src.id().unwrap();
    assert!(alive(pid));
    assert!(src.cancel().is_some(), "not reaped");
    assert!(!alive(pid), "the adapter survived cancel");
    let got = trusty_okf::import::ImportSource::fetch(&mut src, "c1");
    assert_eq!(got.unwrap_err().code, "cancelled");

    fake.flag("hang_list", "");
    let limits = AdapterLimits {
        request: Duration::from_millis(500),
        ..AdapterLimits::default()
    };
    let mut src = AdapterSource::spawn(&fake.cmd(), &mail_spec(), limits).unwrap();
    let pid = src.id().unwrap();
    let listed = trusty_okf::import::ImportSource::list(&mut src, None);
    assert!(format!("{listed:?}").contains("timeout"), "{listed:?}");
    assert!(!alive(pid), "a timed-out adapter was not reaped");
}

/// Why: criterion 98, host half (Appendix B "Acquire role") — every
/// `acquire` request carries the store selection and the run's
/// `operator_initiated`, false unless the caller set it. The adapter half
/// (`in:sent` kept, the grant review) lives with the adapter in
/// trusty-agents.
/// What: the requests the fake logged for a default and an
/// operator-initiated run.
/// Test: self-contained.
#[test]
fn c98_acquire_requests_carry_operator_initiated() {
    let fake = FakeAdapter::new("gmail");
    three_pages(&fake);
    run(&fake, &mail_spec()).unwrap();
    let acquires: Vec<Value> = fake
        .requests()
        .into_iter()
        .filter(|r| r["type"] == "acquire")
        .collect();
    assert!(!acquires.is_empty());
    for r in &acquires {
        assert_eq!(r["operator_initiated"], false, "{r}");
        assert_eq!(r["selection"]["query"], "in:sent", "{r}");
        assert_eq!(r["selection"]["account"], "work", "{r}");
    }
    fake.clear_requests();
    let mut sp = mail_spec();
    sp.operator_initiated = true;
    run(&fake, &sp).unwrap();
    for r in fake.requests().iter().filter(|r| r["type"] == "acquire") {
        assert_eq!(r["operator_initiated"], true, "{r}");
    }
}

/// Why: criterion 132 (row 3) — no tombstone rests on a listing the engine
/// cannot show is whole.
/// What: docs at exactly 5,000 items with a page token left; a 5,000
/// ceiling with no token; a mail listing that exhausts three pages; and a
/// recursive docs control below the ceiling that tombstones its one
/// vanished item.
/// Test: self-contained.
#[test]
fn c132_listing_completeness_gates_tombstones() {
    let fake = FakeAdapter::new("gdocs");
    fake.page(
        None,
        json!([{"item_id": "gone", "revision": "1"}]),
        None,
        true,
    );
    fake.item(json!({"item_id": "gone", "revision": "1", "parts": [{"mime": "text/markdown", "data": "Gone."}]}));
    run(&fake, &docs_spec(true)).unwrap();
    let many: Vec<Value> = (0..5000)
        .map(|i| json!({"item_id": format!("f{i}"), "revision": "1"}))
        .collect();
    fake.page(None, json!(many), Some("more"), true);
    let mut sp = docs_spec(true);
    sp.max_items = 5000;
    sp.exclude = vec!["*".into()];
    let r = run(&fake, &sp).unwrap();
    assert!(!r.pagination_exhausted && !r.deletion_safe, "token left");
    assert_eq!(
        (r.tombstoned, item(&r, "gone").write.as_str()),
        (0, "missing")
    );
    fake.page(None, json!(many), None, true);
    let r = run(&fake, &sp).unwrap();
    assert!(
        !r.pagination_exhausted && !r.deletion_safe,
        "ceiling reached"
    );
    assert_eq!(r.tombstoned, 0);

    let mail = FakeAdapter::new("gmail");
    three_pages(&mail);
    mail.item(msg("m7", "t4", 1_767_225_600_000, "x", "Seven."));
    mail.page(
        Some("p3"),
        json!([
            msg_entry("m5", "t3"),
            msg_entry("m6", "t3"),
            msg_entry("m7", "t4")
        ]),
        None,
        true,
    );
    let mut sp = mail_spec();
    sp.store.tombstone_deleted = true;
    run(&mail, &sp).unwrap();
    mail.page(
        Some("p3"),
        json!([msg_entry("m5", "t3"), msg_entry("m6", "t3")]),
        None,
        true,
    );
    let r = run(&mail, &sp).unwrap();
    assert!(r.pagination_exhausted && !r.deletion_safe, "{r:?}");
    assert_eq!(
        (r.tombstoned, item(&r, "m7").write.as_str()),
        (0, "missing")
    );

    fake.page(
        None,
        json!([{"item_id": "kept", "revision": "1"}]),
        None,
        true,
    );
    fake.item(json!({"item_id": "kept", "revision": "1", "parts": [{"mime": "text/markdown", "data": "Kept."}]}));
    let r = run(&fake, &docs_spec(true)).unwrap();
    assert!(r.pagination_exhausted && r.deletion_safe, "{r:?}");
    assert_eq!(
        (r.tombstoned, item(&r, "gone").write.as_str()),
        (1, "tombstoned")
    );
}

/// Why: criterion 139 (row 11), adapter half — a spawn through the adapter
/// API yields exactly the caller's environment list; trusty-okf adds no
/// variable.
/// What: `/usr/bin/env` spawned through `AdapterCommand::spawn` with a list
/// of three variables, and with an empty list.
/// Test: self-contained.
#[test]
fn c139_adapter_gets_exactly_the_caller_environment() {
    let capture = |list: &[(&str, &str)]| -> Vec<String> {
        let mut cmd = AdapterCommand::new("/usr/bin/env");
        cmd.environment = list
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        let child = cmd.spawn().unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let mut lines: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        lines.sort();
        lines
    };
    let list = [
        ("HOME", "/home/adapter"),
        ("OKF_CRED_STORE", "/run/creds"),
        ("HTTPS_PROXY", "http://proxy:3128"),
    ];
    let mut want: Vec<String> = list.iter().map(|(k, v)| format!("{k}={v}")).collect();
    want.sort();
    assert_eq!(capture(&list), want);
    assert!(capture(&[]).is_empty(), "trusty-okf added a variable");
}
