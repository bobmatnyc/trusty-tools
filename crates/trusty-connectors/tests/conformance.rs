//! Adapter conformance: the slice S9 criteria (SPEC-OKF-01 §6.2 86-98, 132,
//! 144) run against the real `okf-adapter-gmail` and `okf-adapter-gdocs`
//! binaries, with the Google HTTP layer stubbed. Slice S13 of #10746.
//!
//! Why: S9 proved the host against a fake adapter process
//! (`crates/trusty-okf/tests/adapter_criteria.rs`). These tests prove the
//! real adapters against the same host: the same `AdapterSource`,
//! `plan_import` and `import` calls, and the `trusty-okf` CLI entry point,
//! with only Google replaced by a loopback fake (`support::FakeGoogle`).
//! The adapter reads its credential from the token store under the `HOME`
//! the caller lists, and from nothing else.
//! What: one test per criterion half that lives in the adapter, plus the
//! adapter's own failure paths: missing credentials, a failed page, stdin
//! EOF mid-request and cancel.
//! Test: `cargo test -p trusty-connectors --test conformance`.

#![cfg(unix)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{
    FakeGoogle, GDOCS, GMAIL, Home, doc, docs, fm_str, folder, frontmatter, gone, item, mail,
    message, processes_with, read, run, s, spec, tree,
};
use trusty_okf::OkfError;
use trusty_okf::import::adapter::{AdapterLimits, AdapterSource};
use trusty_okf::import::{ContentPolicy, ImportSource, RunCancel};

const JAN1: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

/// Six SENT messages in three threads, two per page.
fn mailbox(fake: &FakeGoogle) {
    fake.with(|st| {
        st.page_cap = Some(2);
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
            st.messages.push(message(
                m,
                t,
                JAN1 + i as i64 * 1000,
                "Hi",
                &format!("Body {m}."),
                true,
            ));
        }
    });
}

/// Concept files only, without the bundle's own state.
fn concepts(root: &std::path::Path) -> Vec<String> {
    tree(root)
        .into_keys()
        .filter(|k| k.ends_with(".md") && !k.starts_with('_') && k != "log.md" && k != "index.md")
        .collect()
}

/// Why: criterion 86 — no account or selection is refused before any
/// adapter starts; 144 — a run with no child processes refuses a `gmail`
/// store before any spawn.
/// What: the real binary's command, three refused specs; the fake sees no
/// request and no adapter process exists.
/// Test: self-contained.
#[test]
fn c86_c144_refusals_happen_before_any_spawn() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let no_account = spec("m", "gmail", &[("query", s("in:sent"))]);
    assert!(matches!(
        AdapterSource::spawn(&cmd, &no_account, AdapterLimits::default()),
        Err(OkfError::SelectionRequired(_))
    ));
    let no_query = spec("m", "gmail", &[("account", s("work"))]);
    assert!(matches!(
        AdapterSource::spawn(&cmd, &no_query, AdapterLimits::default()),
        Err(OkfError::SelectionRequired(_))
    ));
    let mut sub = mail("x", &[]);
    sub.child_processes = false;
    assert!(matches!(
        AdapterSource::spawn(&cmd, &sub, AdapterLimits::default()),
        Err(OkfError::PolicyRefused { .. })
    ));
    assert!(fake.seen().is_empty());
    assert!(processes_with(&fake.url).is_empty());
    assert!(concepts(&h.root).is_empty());
}

/// Why: criteria 87 and 132 — the adapter follows every page; at its own
/// ceiling it returns the page token it did not follow with `complete:
/// false`, so the report says `pagination_exhausted: false` with the cursor
/// and nothing is tombstoned.
/// What: six messages, two per page; a full run, then a run with
/// `max_items = 4`.
/// Test: self-contained.
#[test]
fn c87_c132_every_page_imports_and_the_adapter_ceiling_is_never_complete() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    mailbox(&fake);
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let mut sp = mail("from:me", &[]);
    sp.store.tombstone_deleted = true;
    let r = run(&cmd, &h.root, &sp).unwrap();
    assert_eq!(r.created, 6, "{r:?}");
    assert!(r.pagination_exhausted && !r.deletion_safe, "{r:?}");
    assert_eq!(fake.list_queries().len(), 3, "three pages");

    let h2 = Home::new(Some("work"));
    fake.clear_seen();
    let cut = mail("from:me", &[("max_items", s("4"))]);
    let r = run(&h2.cmd(GMAIL, &fake, &[]), &h2.root, &cut).unwrap();
    assert_eq!(r.listed, 4, "{r:?}");
    assert!(!r.pagination_exhausted && !r.deletion_safe, "{r:?}");
    assert_eq!(
        r.cursor.as_deref(),
        Some("o4"),
        "the token left is reported"
    );
    assert_eq!(r.tombstoned, 0);
    assert_eq!(
        fake.list_queries().len(),
        2,
        "no Google call past the ceiling"
    );
}

/// Why: criteria 88 and 89 — concept IDs come from message and thread IDs,
/// never subjects; each concept keeps `thread_id`, `internalDate` and the
/// `Date` header, and the thread concept links its messages in date order.
/// What: two same-day messages with one subject in one thread.
/// Test: self-contained.
#[test]
fn c88_c89_ids_from_source_ids_and_thread_dates_kept() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.messages
            .push(message("late", "t1", JAN1 + 60_000, "Same", "Later.", true));
        st.messages
            .push(message("early", "t1", JAN1, "Same", "Earlier.", true));
    });
    run(&h.cmd(GMAIL, &fake, &[]), &h.root, &mail("x", &[])).unwrap();
    let fm = frontmatter(&h.root, "mail/mail-src/t1/early.md");
    assert_eq!(fm_str(&fm, "thread_id").as_deref(), Some("t1"));
    assert_eq!(
        fm_str(&fm, "internal_date").as_deref(),
        Some("1767225600000")
    );
    assert_eq!(
        fm_str(&fm, "date_header").as_deref(),
        Some("Thu, 1 Jan 2026 00:00:00 +0000")
    );
    assert!(h.root.join("mail/mail-src/t1/late.md").exists());
    let thread = read(&h.root, "mail/mail-src/t1.md");
    let e = thread
        .find("(/mail/mail-src/t1/early.md)")
        .expect("early linked");
    let l = thread
        .find("(/mail/mail-src/t1/late.md)")
        .expect("late linked");
    assert!(e < l, "not in date order:\n{thread}");
}

/// Why: criterion 90 — an unchanged mailbox writes zero bytes, and a
/// widened window fetches only the new message.
/// What: two runs with `after = 2026/01/01`, then an older message and a
/// run with `after = 2025/12/01`.
/// Test: self-contained.
#[test]
fn c90_unchanged_mailbox_writes_nothing_and_a_wider_window_fetches_only_new() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    mailbox(&fake);
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let jan = mail("from:me", &[("after", s("2026/01/01"))]);
    run(&cmd, &h.root, &jan).unwrap();
    let before = tree(&h.root);
    fake.clear_seen();
    let r = run(&cmd, &h.root, &jan).unwrap();
    assert_eq!((r.written, r.unchanged), (0, 6), "{r:?}");
    assert!(fake.fetched().is_empty(), "{:?}", fake.fetched());
    assert_eq!(tree(&h.root), before, "a repeat import wrote bytes");

    fake.with(|st| {
        st.messages.push(message(
            "m0",
            "t9",
            JAN1 - 86_400_000,
            "Old",
            "Older.",
            true,
        ))
    });
    fake.clear_seen();
    let dec = mail("from:me", &[("after", s("2025/12/01"))]);
    let r = run(&cmd, &h.root, &dec).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert_eq!(fake.fetched(), vec!["m0".to_string()]);
}

/// Why: criterion 91 — a Markdown or HTML export keeps headings, lists,
/// tables and links; a binary (base64) export is `unsupported_format`;
/// embedded images are dropped with a note.
/// What: three Docs: a Markdown export with a data-URI image, an
/// HTML-only export, and a Markdown export that is not UTF-8.
/// Test: self-contained.
#[test]
fn c91_docs_exports_keep_structure_and_base64_is_unsupported() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    let md = "# Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n[site](https://example.com)\n\n\
              [image1]: <data:image/png;base64,AAAA>\n";
    let html = "<h1>Title</h1><ul><li>one</li></ul><table><tr><td>1</td></tr></table>\
                <a href=\"https://example.com\">site</a>";
    fake.with(|st| {
        for id in ["md", "html", "bin"] {
            st.files.push(doc(id, "F", 1));
        }
        st.exports.insert(
            ("md".into(), "text/markdown".into()),
            md.as_bytes().to_vec(),
        );
        st.exports.insert(
            ("html".into(), "text/html".into()),
            html.as_bytes().to_vec(),
        );
        st.exports.insert(
            ("bin".into(), "text/markdown".into()),
            vec![0xff, 0xfe, 0x00, 0x81],
        );
    });
    let r = run(&h.cmd(GDOCS, &fake, &[]), &h.root, &docs(false, &[])).unwrap();
    let body = read(&h.root, "docs/docs-src/md.md");
    for want in [
        "# Title",
        "- two",
        "| 1 | 2 |",
        "[site](https://example.com)",
    ] {
        assert!(body.contains(want), "{want} missing:\n{body}");
    }
    assert!(!body.contains("data:image"), "{body}");
    let body = read(&h.root, "docs/docs-src/html.md");
    assert!(
        body.contains("Title") && body.contains("example.com"),
        "{body}"
    );
    assert_eq!(
        item(&r, "bin").reason.as_deref(),
        Some("unsupported_format"),
        "{r:?}"
    );
    assert!(!h.root.join("docs/docs-src/bin.md").exists());
}

/// Why: criterion 92 — a message with no decodable text part is
/// `degraded: snippet_only` and writes no concept by default.
/// What: a message whose only part is an attachment.
/// Test: self-contained.
#[test]
fn c92_snippet_only_message_is_degraded() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    let mut m = message("s1", "t1", JAN1, "Hi", "x", true);
    m["payload"]["parts"] =
        json!([{"mimeType": "image/png", "filename": "a.png", "body": {"size": 5}}]);
    fake.with(|st| st.messages.push(m));
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let r = run(&cmd, &h.root, &mail("x", &[])).unwrap();
    assert_eq!(
        item(&r, "s1").degraded.as_deref(),
        Some("snippet_only"),
        "{r:?}"
    );
    assert_eq!(r.written, 0);
    let mut sp = mail("x", &[]);
    sp.policy = ContentPolicy::AllowSnippet;
    let r = run(&cmd, &h.root, &sp).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert!(read(&h.root, "mail/mail-src/t1/s1.md").contains("snip"));
}

/// Why: criterion 93 — no raw MIME is read and no attachment is
/// downloaded; attachments are listed by name, type and size; quoted text
/// stays.
/// What: a reply with quoted text and a PDF attachment.
/// Test: self-contained.
#[test]
fn c93_attachments_listed_quotes_kept_no_raw_mime() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    let mut m = message(
        "q1",
        "t1",
        JAN1,
        "Re: plan",
        "Agreed.\n\n> On Monday you wrote:\n> the plan",
        true,
    );
    m["payload"]["parts"].as_array_mut().unwrap().push(
        json!({"mimeType": "application/pdf", "filename": "deck.pdf",
                     "body": {"attachmentId": "A1", "size": 2048}}),
    );
    fake.with(|st| st.messages.push(m));
    run(&h.cmd(GMAIL, &fake, &[]), &h.root, &mail("x", &[])).unwrap();
    let body = read(&h.root, "mail/mail-src/t1/q1.md");
    assert!(
        body.contains("> On Monday you wrote:\n> the plan"),
        "{body}"
    );
    assert!(
        body.contains("- `deck.pdf` (application/pdf, 2048 bytes)"),
        "{body}"
    );
    for seen in fake.seen() {
        assert!(!seen.path.contains("/attachments/"), "{seen:?}");
        assert_ne!(
            seen.query.get("format").map(String::as_str),
            Some("raw"),
            "{seen:?}"
        );
    }
}

/// Why: criterion 94 — a trashed Doc on a complete recursive listing is
/// tombstoned and reported.
/// What: a Doc in a subfolder is imported, then trashed.
/// Test: self-contained.
#[test]
fn c94_trashed_doc_on_a_complete_recursive_listing_is_tombstoned() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.files.push(doc("d1", "F", 1));
        st.files.push(folder("S", "F"));
        st.files.push(doc("d2", "S", 1));
        for d in ["d1", "d2"] {
            st.exports
                .insert((d.into(), "text/markdown".into()), b"Doc.\n".to_vec());
        }
    });
    let cmd = h.cmd(GDOCS, &fake, &[]);
    let r = run(&cmd, &h.root, &docs(true, &[])).unwrap();
    assert_eq!(r.created, 2, "{r:?}");
    fake.with(|st| st.files[2]["trashed"] = json!(true));
    let r = run(&cmd, &h.root, &docs(true, &[])).unwrap();
    assert!(r.pagination_exhausted && r.deletion_safe, "{r:?}");
    assert_eq!(item(&r, "d2").write, "tombstoned");
    let fm = frontmatter(&h.root, "docs/docs-src/d2.md");
    assert_eq!(fm_str(&fm, "source_status").as_deref(), Some("deleted"));
}

/// Why: criterion 95 — a run interrupted mid-chunk resumes from the ledger
/// and re-fetches at most one item; criterion 97 — a cancel kills and
/// reaps the adapter.
/// What: the fake holds the third fetch; the run is cancelled there; the
/// adapter is gone; a re-run fetches only the items the ledger lacks.
/// Test: self-contained.
#[test]
fn c95_c97_cancelled_run_reaps_the_adapter_and_resumes_from_the_ledger() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    mailbox(&fake);
    fake.with(|st| st.hold = Some("messages/m3".into()));
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let flag = Arc::new(AtomicBool::new(false));
    let mut sp = mail("x", &[]);
    sp.cancel = RunCancel::new(Arc::clone(&flag));
    let (cmd2, root2, sp2) = (cmd.clone(), h.root.clone(), sp.clone());
    let worker = std::thread::spawn(move || run(&cmd2, &root2, &sp2));
    fake.wait_for("messages/m3");
    assert_eq!(processes_with(&fake.url).len(), 1, "the adapter runs");
    flag.store(true, Ordering::SeqCst);
    let _ = worker.join().unwrap();
    assert!(
        gone(&fake.url),
        "adapter left after cancel: {:?}",
        processes_with(&fake.url)
    );
    let first = fake.fetched();
    fake.with(|st| st.hold = None);
    fake.clear_seen();
    run(&cmd, &h.root, &mail("x", &[])).unwrap();
    let again = fake.fetched();
    let refetched = again.iter().filter(|i| first.contains(i)).count();
    assert!(
        refetched <= 1,
        "re-fetched {refetched}: {first:?} then {again:?}"
    );
    assert_eq!(
        concepts(&h.root)
            .iter()
            .filter(|c| c.matches('/').count() == 3)
            .count(),
        6
    );
}

/// Why: criterion 97 — the `trusty-okf` CLI imports through the real
/// adapter end to end, and no adapter process outlives the run.
/// What: `trusty_okf::cli::run` with `--from adapter:gmail`, the binary,
/// `--adapter-arg` for the fake and `--adapter-var HOME=...`.
/// Test: self-contained.
#[test]
fn c97_cli_imports_through_the_real_adapter() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    mailbox(&fake);
    let args: Vec<std::ffi::OsString> = [
        "import",
        "run",
        h.root.to_str().unwrap(),
        "--from",
        "adapter:gmail",
        "--adapter",
        GMAIL,
        "--adapter-arg",
        "--api-base",
        "--adapter-arg",
        &fake.url,
        "--adapter-var",
        &format!("HOME={}", h.home.display()),
        "--select",
        "account=work",
        "--select",
        "query=from:me",
        "--json",
        "--now",
        "2026-11-04T12:00:00Z",
    ]
    .iter()
    .map(Into::into)
    .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = trusty_okf::cli::run(&args, &mut out, &mut err);
    let report: Value = serde_json::from_slice(&out).unwrap_or(Value::Null);
    assert_eq!(code, 0, "{report}\n{}", String::from_utf8_lossy(&err));
    assert_eq!(report["created"], 6, "{report}");
    assert!(
        gone(&fake.url),
        "adapter left: {:?}",
        processes_with(&fake.url)
    );
}

/// Why: the adapter exits when its host closes stdin, even while a Google
/// call is in flight, so a vanished host leaves no adapter behind.
/// What: the binary is spawned directly; after the handshake a `list` is
/// held at the fake and stdin is closed; the adapter exits 0 within 5 s.
/// A `shutdown` frame also exits 0.
/// Test: self-contained.
#[test]
fn stdin_eof_mid_request_ends_the_adapter() {
    use std::io::Write;
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| st.hold = Some("/messages?".into()));
    let frame = |v: Value| {
        let b = v.to_string();
        let mut f = (b.len() as u32).to_be_bytes().to_vec();
        f.extend_from_slice(b.as_bytes());
        f
    };
    let hello = json!({"type": "hello", "role": "acquire", "protocol_min": 1, "protocol_max": 1});
    let list = json!({"type": "acquire", "request_id": 1, "op": "list", "kind": "gmail",
                      "store": "m", "selection": {"account": "work", "query": "x"}});
    for (second, wait) in [(list, true), (json!({"type": "shutdown"}), false)] {
        let mut child = h.cmd(GMAIL, &fake, &[]).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&frame(hello.clone())).unwrap();
        stdin.write_all(&frame(second)).unwrap();
        stdin.flush().unwrap();
        if wait {
            fake.wait_for("/messages");
        }
        drop(stdin);
        let until = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            assert!(Instant::now() < until, "the adapter did not exit on EOF");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(0));
    }
    fake.with(|st| st.hold = None);
}

/// Why: criterion 98, not operator-initiated — every listing query carries
/// `in:sent`, no caller query removes it, and a message without the SENT
/// label is refused even when the listing returns it.
/// What: an inbound-looking query; then a fake that ignores `in:sent`.
/// Test: self-contained.
#[test]
fn c98_sent_only_holds_in_the_query_and_per_message() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.messages
            .push(message("out", "t1", JAN1, "Out", "Sent.", true));
        st.messages
            .push(message("in", "t2", JAN1, "In", "Inbound.", false));
    });
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let r = run(&cmd, &h.root, &mail("in:inbox OR -in:sent", &[])).unwrap();
    for q in fake.list_queries() {
        assert_eq!(q, "in:sent (in:inbox OR -in:sent)");
    }
    assert_eq!(r.listed, 1, "{r:?}");
    assert!(run(&cmd, &h.root, &mail("x) OR (in:inbox", &[])).is_err());

    fake.with(|st| st.ignore_sent = true);
    let r = run(&cmd, &h.root, &mail("x", &[])).unwrap();
    assert_eq!(
        item(&r, "in").reason.as_deref(),
        Some("policy_refused"),
        "{r:?}"
    );
    assert!(!h.root.join("mail/mail-src/t2/in.md").exists());
}

/// Why: criterion 98, inbound — an inbound read is refused unless the run
/// is operator-initiated and the adapter's grant file holds a passed
/// review for the store; after it, inbound mail imports as
/// `UntrustedExternal`.
/// What: `mailbox = all` without the operator mark, with it but no grant,
/// then with both.
/// Test: self-contained.
#[test]
fn c98_inbound_needs_an_operator_run_and_a_grant() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.messages
            .push(message("in", "t2", JAN1, "In", "Inbound.", false))
    });
    let cmd = h.cmd(GMAIL, &fake, &[]);
    let all = mail("in:inbox", &[("mailbox", s("all"))]);
    assert!(run(&cmd, &h.root, &all).is_err(), "not operator-initiated");
    let mut op = all.clone();
    op.operator_initiated = true;
    assert!(run(&cmd, &h.root, &op).is_err(), "no grant");
    assert!(fake.list_queries().is_empty(), "no listing before the gate");
    assert!(concepts(&h.root).is_empty());
    h.grant("mail-src", "work");
    let r = run(&cmd, &h.root, &op).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert_eq!(fake.list_queries(), vec!["in:inbox".to_string()]);
    let fm = frontmatter(&h.root, "mail/mail-src/t2/in.md");
    assert_eq!(fm_str(&fm, "trust").as_deref(), Some("untrusted-external"));
}

/// Why: criterion 132 — a recursive Docs listing cut at the adapter's
/// ceiling is not deletion-safe and tombstones nothing; under the ceiling
/// it is, and the vanished Doc is tombstoned.
/// What: five Docs imported; one removed; a run with `max_items = 3`, then
/// one without.
/// Test: self-contained.
#[test]
fn c132_docs_ceiling_blocks_tombstones_and_a_whole_listing_allows_them() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.page_cap = Some(2);
        for i in 0..5 {
            let id = format!("d{i}");
            st.files.push(doc(&id, "F", 1));
            st.exports
                .insert((id, "text/markdown".into()), b"Doc.\n".to_vec());
        }
    });
    let cmd = h.cmd(GDOCS, &fake, &[]);
    assert_eq!(run(&cmd, &h.root, &docs(true, &[])).unwrap().created, 5);
    fake.with(|st| st.files.retain(|f| f["id"] != "d4"));
    let r = run(&cmd, &h.root, &docs(true, &[("max_items", s("3"))])).unwrap();
    assert!(!r.pagination_exhausted && !r.deletion_safe, "{r:?}");
    assert!(
        r.cursor.as_deref().is_some_and(|c| c.starts_with("w1:")),
        "{r:?}"
    );
    assert_eq!(
        (r.tombstoned, item(&r, "d4").write.as_str()),
        (0, "missing")
    );
    let r = run(&cmd, &h.root, &docs(true, &[])).unwrap();
    assert!(r.pagination_exhausted && r.deletion_safe, "{r:?}");
    assert_eq!(
        (r.tombstoned, item(&r, "d4").write.as_str()),
        (1, "tombstoned")
    );
}

/// Why: Docs named by ID list in pages; a missing one fails at its fetch,
/// never silently.
/// What: `document_ids = d1, gone`; a non-Doc file fails `unsupported_format`.
/// Test: self-contained.
#[test]
fn document_ids_list_and_a_missing_one_fails_per_item() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        st.files.push(doc("d1", "F", 1));
        st.exports
            .insert(("d1".into(), "text/markdown".into()), b"One.\n".to_vec());
    });
    let ids = toml::Value::Array(vec![s("d1"), s("gone")]);
    let sp = spec(
        "docs-src",
        "gdocs",
        &[("account", s("work")), ("document_ids", ids)],
    );
    let r = run(&h.cmd(GDOCS, &fake, &[]), &h.root, &sp).unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    assert_eq!(item(&r, "gone").read, "failed", "{r:?}");
    assert!(!r.deletion_safe);
}

/// Why: credentials come only from the adapter's own store under the
/// caller's `HOME`; with no token, or a token Google rejects, the run fails
/// closed with nothing written.
/// What: an empty store; then a store for another account; then a fake
/// that rejects the token on fetch.
/// Test: self-contained.
#[test]
fn missing_credentials_fail_closed_before_any_write() {
    let fake = FakeGoogle::start();
    mailbox(&fake);
    for account in [None, Some("other")] {
        let h = Home::new(account);
        let e = run(&h.cmd(GMAIL, &fake, &[]), &h.root, &mail("x", &[])).unwrap_err();
        assert!(e.to_string().contains("auth_required"), "{e}");
        assert!(concepts(&h.root).is_empty());
    }
    assert!(fake.seen().is_empty(), "no request without a credential");
    let h = Home::new(Some("work"));
    fake.with(|st| st.fail.push(("messages/m1".into(), 401)));
    let r = run(&h.cmd(GMAIL, &fake, &[]), &h.root, &mail("x", &[])).unwrap();
    assert_eq!(item(&r, "m1").read, "failed", "{r:?}");
    assert!(!h.root.join("mail/mail-src/t1/m1.md").exists());
    assert!(
        fake.seen()
            .iter()
            .all(|s| s.auth.as_deref() == Some("Bearer stub-access-token"))
    );
}

/// Why: the credential comes only from the user-tier store under the
/// caller's `HOME`. A `./.gworkspace-mcp/tokens.json` in the directory the
/// adapter starts in (the trusty-okf CLI sets none) must never be read, even
/// though trusty-gworkspace's default store lets such a project entry win
/// (#8539). #10746 S13 fix round.
/// What: a working directory whose store holds a token for the selected
/// account while `HOME` holds none fails `auth_required` with no request;
/// a working directory whose store holds another token for the same account
/// is ignored, and only the `HOME` token reaches Google.
/// Test: self-contained.
#[test]
fn a_cwd_token_store_is_never_read() {
    let fake = FakeGoogle::start();
    mailbox(&fake);
    let plant = |dir: &std::path::Path, token: &str| {
        std::fs::create_dir_all(dir.join(".gworkspace-mcp")).unwrap();
        let store = json!({"work": {
            "metadata": {"service_name": "work", "created_at": "2026-01-01T00:00:00Z"},
            "token": {"access_token": token, "expires_at": "2099-01-01T00:00:00Z"}
        }});
        std::fs::write(dir.join(".gworkspace-mcp/tokens.json"), store.to_string()).unwrap();
    };
    let h = Home::new(Some("other"));
    let cwd = h.tmp.path().join("project");
    plant(&cwd, support::TOKEN);
    let mut cmd = h.cmd(GMAIL, &fake, &[]);
    cmd.current_dir = Some(cwd.clone());
    let e = run(&cmd, &h.root, &mail("x", &[])).unwrap_err();
    assert!(e.to_string().contains("auth_required"), "{e}");
    assert!(
        fake.seen().is_empty(),
        "a cwd token was used: {:?}",
        fake.seen()
    );

    let h = Home::new(Some("work"));
    let cwd = h.tmp.path().join("project");
    plant(&cwd, "cwd-token");
    let mut cmd = h.cmd(GMAIL, &fake, &[]);
    cmd.current_dir = Some(cwd);
    let r = run(&cmd, &h.root, &mail("x", &[])).unwrap();
    assert_eq!(r.created, 6, "{r:?}");
    assert!(
        fake.seen()
            .iter()
            .all(|s| s.auth.as_deref() == Some("Bearer stub-access-token")),
        "{:?}",
        fake.seen()
    );
}

/// Why: a listing that fails part way is an error, never a short listing
/// that reads as whole.
/// What: the second page answers 500; the run fails and writes nothing.
/// Test: self-contained.
#[test]
fn a_failed_page_fails_the_run_and_writes_nothing() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    mailbox(&fake);
    fake.with(|st| st.fail.push(("pageToken=o2".into(), 500)));
    let e = run(&h.cmd(GMAIL, &fake, &[]), &h.root, &mail("x", &[])).unwrap_err();
    assert!(e.to_string().contains("fetch_error"), "{e}");
    assert!(concepts(&h.root).is_empty());
    assert!(fake.fetched().is_empty());
}

/// Why: a cancel kills the adapter's process group at once, even while
/// the adapter waits on Google inside a `list`.
/// What: the first list call is held at the fake; the run's cancel flag is
/// set; the call fails and the adapter is gone.
/// Test: self-contained.
#[test]
fn cancel_while_waiting_on_google_kills_the_adapter() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| st.hold = Some("/messages?".into()));
    let flag = Arc::new(AtomicBool::new(false));
    let mut sp = mail("x", &[]);
    sp.cancel = RunCancel::new(Arc::clone(&flag));
    let mut src =
        AdapterSource::spawn(&h.cmd(GMAIL, &fake, &[]), &sp, AdapterLimits::default()).unwrap();
    let pid = src.id().unwrap();
    let lister = std::thread::spawn(move || src.list(None).map(|_| ()));
    fake.wait_for("/messages");
    assert!(processes_with(&fake.url).contains(&pid));
    let started = Instant::now();
    flag.store(true, Ordering::SeqCst);
    assert!(lister.join().unwrap().is_err(), "cancelled");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(gone(&fake.url), "{:?}", processes_with(&fake.url));
    fake.with(|st| st.hold = None);
}

/// Why: #10746 review (PR 10821) — Drive answers `files.list` for an
/// unknown or unshared folder with 200 and an empty page, so a walk that
/// never checks its root reads as a whole, empty listing and tombstones
/// every item of the store.
/// What: two Docs under `F` imported; then `F` unknown (no root, no
/// files), trashed, not a folder, and refused with 403. Each run fails
/// before any write, with `not_found` and `permission_denied` for the HTTP
/// cases; the Docs stay live.
/// Test: self-contained.
#[test]
fn an_unknown_or_unusable_root_fails_the_run_and_tombstones_nothing() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("work")));
    fake.with(|st| {
        for d in ["d1", "d2"] {
            st.files.push(doc(d, "F", 1));
            st.exports
                .insert((d.into(), "text/markdown".into()), b"Doc.\n".to_vec());
        }
    });
    let cmd = h.cmd(GDOCS, &fake, &[]);
    assert_eq!(run(&cmd, &h.root, &docs(true, &[])).unwrap().created, 2);
    type Set = fn(&mut support::State);
    let cases: [(&str, Set); 4] = [
        ("not_found", |st| {
            st.roots.clear();
            st.files.clear();
        }),
        ("trashed", |st| {
            st.roots = vec![folder("F", "root")];
            st.roots[0]["trashed"] = json!(true);
        }),
        ("not a folder", |st| st.roots = vec![doc("F", "root", 1)]),
        ("permission_denied", |st| {
            st.roots = vec![folder("F", "root")];
            st.fail.push(("drive/v3/files/F?".into(), 403));
        }),
    ];
    for (want, set) in cases {
        fake.with(set);
        let before = tree(&h.root);
        let got = run(&cmd, &h.root, &docs(true, &[]));
        let e = got.map(|r| (r.tombstoned, r.deletion_safe)).unwrap_err();
        assert!(e.to_string().contains(want), "{want}: {e}");
        assert_eq!(tree(&h.root), before, "{want}: the run wrote");
    }
    for d in ["d1", "d2"] {
        let fm = frontmatter(&h.root, &format!("docs/docs-src/{d}.md"));
        assert_ne!(
            fm_str(&fm, "source_status").as_deref(),
            Some("deleted"),
            "{d}"
        );
    }
}
