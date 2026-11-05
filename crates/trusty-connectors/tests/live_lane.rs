//! The S13 live lane: SPEC-OKF-01 criterion 148 against a real Google test
//! account, gated on `OKF_LIVE_PROFILE`. Slice S13 of #10746, ok-T21.
//!
//! Why: the conformance suite proves the adapters against a stubbed Google.
//! Criterion 148 asks for the same outcomes from a real account holding the
//! S13 fixtures, under a token that can only read. The lane lives here, not
//! in trusty-okf, because it needs the adapter binaries and the credential
//! side: the token store, the `BaseClient` and tokeninfo. trusty-okf links
//! none of that (B1, B10; `scripts/check_okf_boundary.sh`).
//! What: `live_lane_against_the_owner_account` does nothing but print that
//! it was skipped unless `OKF_LIVE_PROFILE` names a token-store profile.
//! With it set, `run_lane` first runs `trusty_connectors::live::precheck`
//! (tokeninfo; any scope beyond the read-only preset fails the test before
//! any import), then imports the fixtures with `plan_import` and `import`
//! (the library form of `trusty-okf import plan` / `import run`) through the
//! real `okf-adapter-gmail` and `okf-adapter-gdocs`, and checks outcomes
//! (1) to (8). The profile is never defaulted: it is the account of every
//! selection and the only profile the pre-check reads. The other tests run
//! the same `run_lane` offline, against the loopback fake with fixtures
//! shaped like the test account's.
//!
//! # Running the live lane (owner channel only)
//!
//! A tm session's egress guard (bobmatnyc/trusty-tools#8454) blocks Gmail,
//! so the lane runs only in the owner channel (D89), in a plain terminal:
//!
//! 1. Mint the read-only profile once (ok-T20):
//!    `trusty-gworkspace-mcp setup --profile r-1mc-readonly --scopes readonly --no-default`.
//! 2. Run, with the fixture folder's Drive ID:
//!    `OKF_LIVE_PROFILE=r-1mc-readonly OKF_LIVE_DRIVE_FOLDER=<folder id>
//!    cargo test -p trusty-connectors --test live_lane -- --nocapture --test-threads=1`
//!
//! `OKF_LIVE_GMAIL_QUERY` narrows the SENT listing (default `in:sent`; the
//! adapter always adds `in:sent`). `HOME` must hold
//! `~/.gworkspace-mcp/tokens.json` with that profile. The lane writes only
//! to temporary bundles and makes no write to the account. A pre-check
//! refusal fails the test, naming the scope; it never warns and continues.
//!
//! Test: `cargo test -p trusty-connectors --test live_lane`.
//!
//! # Spec References
//! - SPEC-OKF-01 §6.2 criteria 148, 87-94.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use support::{
    FakeGoogle, GDOCS, GMAIL, Home, doc, fm_str, folder, frontmatter, message, run, s, spec, tree,
};
use trusty_connectors::google::Options;
use trusty_connectors::live::precheck;
use trusty_gworkspace::api::constants::READONLY_SCOPES;
use trusty_okf::import::adapter::AdapterCommand;
use trusty_okf::import::{ImportReport, ImportSpec};

const PROFILE_VAR: &str = "OKF_LIVE_PROFILE";
const FOLDER_VAR: &str = "OKF_LIVE_DRIVE_FOLDER";
const QUERY_VAR: &str = "OKF_LIVE_GMAIL_QUERY";
const MAIL_STORE: &str = "lane-mail";
const DOCS_STORE: &str = "lane-docs";
const DAY: i64 = 86_400_000;
const JAN1: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

/// One lane run's inputs.
struct Lane {
    /// The `HOME` the adapters and the pre-check read the token store under.
    home: PathBuf,
    /// The token-store profile, used as every selection's `account`.
    account: String,
    query: String,
    folder: String,
    /// The loopback fake's root (offline runs only).
    api_base: Option<String>,
    /// Holds the bundles; removed on drop.
    scratch: tempfile::TempDir,
}

impl Lane {
    fn new(home: &Path, account: &str, folder: &str, api_base: Option<&str>) -> Lane {
        Lane {
            home: home.to_path_buf(),
            account: account.to_string(),
            query: "in:sent".to_string(),
            folder: folder.to_string(),
            api_base: api_base.map(str::to_string),
            scratch: tempfile::tempdir().unwrap(),
        }
    }

    /// A fresh, empty bundle root.
    fn bundle(&self, name: &str) -> PathBuf {
        let root = self.scratch.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The adapter command: exactly `HOME` in its environment, the fake's
    /// root only offline, and a working directory with no token store.
    fn cmd(&self, bin: &str) -> AdapterCommand {
        let mut c = AdapterCommand::new(bin);
        if let Some(root) = &self.api_base {
            c.args = vec!["--api-base".into(), root.into()];
        }
        c.environment = vec![("HOME".into(), self.home.clone().into())];
        c.current_dir = Some(self.scratch.path().to_path_buf());
        c
    }

    fn mail(&self, max_items: Option<&str>) -> ImportSpec {
        let mut p = vec![("account", s(&self.account)), ("query", s(&self.query))];
        if let Some(n) = max_items {
            p.push(("max_items", s(n)));
        }
        let mut sp = spec(MAIL_STORE, "gmail", &p);
        sp.store.tombstone_deleted = true;
        sp
    }

    fn docs(&self, recursive: bool) -> ImportSpec {
        spec(
            DOCS_STORE,
            "gdocs",
            &[
                ("account", s(&self.account)),
                ("folder_id", s(&self.folder)),
                ("recursive", toml::Value::Boolean(recursive)),
            ],
        )
    }
}

/// Criterion 148: the pre-check, then outcomes (1) to (8). `Err` is a
/// pre-check refusal, returned before any adapter starts.
fn run_lane(l: &Lane) -> Result<(), String> {
    let opts = Options {
        api_base: l.api_base.clone(),
        config: None,
    };
    let scopes = precheck(&l.home, &opts, &l.account)?;
    println!("pre-check passed: {}", scopes.join(" "));
    mail_outcomes(l);
    docs_outcomes(l);
    Ok(())
}

fn import_run(l: &Lane, bin: &str, root: &Path, sp: &ImportSpec) -> ImportReport {
    run(&l.cmd(bin), root, sp).unwrap_or_else(|e| panic!("import failed: {e}"))
}

/// Every concept file of `store` under `kind`, keyed by item ID.
fn concepts(root: &Path, kind: &str, store: &str) -> BTreeMap<String, String> {
    let prefix = format!("{kind}/{store}/");
    tree(root)
        .into_keys()
        .filter_map(|rel| {
            let rest = rel.strip_prefix(&prefix)?.to_string();
            let id = rest.rsplit('/').next()?.strip_suffix(".md")?.to_string();
            Some((id, rel))
        })
        .collect()
}

fn body(root: &Path, rel: &str) -> String {
    let text = std::fs::read_to_string(root.join(rel)).unwrap();
    text.splitn(3, "---\n")
        .nth(2)
        .unwrap_or_default()
        .to_string()
}

/// Outcomes (1) to (5) over the SENT fixtures.
fn mail_outcomes(l: &Lane) {
    let root = l.bundle("mail");
    // (1) criterion 87: a small ceiling leaves the listing open.
    let cut = import_run(l, GMAIL, &root, &l.mail(Some("1")));
    assert!(
        !cut.pagination_exhausted && cut.cursor.is_some() && cut.tombstoned == 0,
        "(1) a cut run must report pagination_exhausted false, a cursor and no tombstone: {cut:?}"
    );
    let full = import_run(l, GMAIL, &root, &l.mail(None));
    assert!(
        full.pagination_exhausted && full.failed == 0,
        "(1) {full:?}"
    );
    let on_disk = concepts(&root, "mail", MAIL_STORE);
    for it in &full.items {
        let imported = on_disk.contains_key(&it.item_id);
        let snippet_only = it.degraded.as_deref() == Some("snippet_only");
        assert!(
            imported || snippet_only,
            "(1) {} not imported: {it:?}",
            it.item_id
        );
    }
    println!(
        "(1) ceiling 1: cursor kept; second run: {} items",
        full.items.len()
    );

    // Messages: (item id, thread, internal date, title, path).
    let msgs: Vec<(String, String, i64, String, String)> = on_disk
        .iter()
        .filter(|(_, rel)| rel.matches('/').count() == 3)
        .map(|(id, rel)| {
            let fm = frontmatter(&root, rel);
            let ms = fm_str(&fm, "internal_date").unwrap().parse().unwrap();
            let thread = fm_str(&fm, "thread_id").unwrap();
            (
                id.clone(),
                thread,
                ms,
                fm_str(&fm, "title").unwrap_or_default(),
                rel.clone(),
            )
        })
        .collect();

    // (2) criterion 88: a same-day pair with one subject is two concepts.
    let mut by_day: BTreeMap<(String, i64), Vec<&str>> = BTreeMap::new();
    for (id, _, ms, title, _) in &msgs {
        by_day
            .entry((title.clone(), ms / DAY))
            .or_default()
            .push(id.as_str());
    }
    let pair = by_day
        .iter()
        .find(|((title, _), ids)| !title.is_empty() && ids.len() >= 2)
        .map(|(_, ids)| ids);
    assert!(
        pair.is_some(),
        "(2) no same-day same-subject pair: {by_day:?}"
    );
    println!("(2) same-day pair: {:?}", pair.unwrap());

    // (3) criterion 89: one directory per thread, linked in date order.
    let mut threads: BTreeMap<&str, Vec<(i64, &str)>> = BTreeMap::new();
    for (id, t, ms, _, rel) in &msgs {
        assert!(
            rel.ends_with(&format!("/{t}/{id}.md")),
            "(3) {rel} not in {t}/"
        );
        threads
            .entry(t.as_str())
            .or_default()
            .push((*ms, id.as_str()));
    }
    let multi: Vec<_> = threads.iter().filter(|(_, m)| m.len() >= 2).collect();
    assert!(!multi.is_empty(), "(3) no thread of two or more messages");
    for (t, members) in &multi {
        let mut members = members.to_vec();
        members.sort();
        let text = std::fs::read_to_string(root.join(format!("mail/{MAIL_STORE}/{t}.md")))
            .unwrap_or_else(|e| panic!("(3) thread concept {t}: {e}"));
        let at: Vec<usize> = members
            .iter()
            .map(|(_, id)| {
                text.find(&format!("(/mail/{MAIL_STORE}/{t}/{id}.md)"))
                    .unwrap_or_else(|| panic!("(3) {t} does not link {id}:\n{text}"))
            })
            .collect();
        assert!(
            at.windows(2).all(|w| w[0] < w[1]),
            "(3) {t} not in date order:\n{text}"
        );
    }
    println!("(3) {} thread(s) linked in date order", multi.len());

    // (4) criterion 93: quoted text kept and marked; attachments listed only.
    let bodies: Vec<String> = msgs.iter().map(|m| body(&root, &m.4)).collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.lines().any(|x| x.starts_with("> "))),
        "(4) no body keeps quoted text marked with `> `"
    );
    let listed = bodies.iter().find(|b| {
        b.split("## Attachments").nth(1).is_some_and(|tail| {
            tail.lines()
                .any(|x| x.starts_with("- `") && x.contains(", ") && x.ends_with(" bytes)"))
        })
    });
    assert!(
        listed.is_some(),
        "(4) no attachment listed by name, type and size"
    );
    println!("(4) quoted reply kept; attachment listed");

    // (5) criterion 92: the message with no text part has no concept.
    let degraded: Vec<_> = full
        .items
        .iter()
        .filter(|i| i.degraded.as_deref() == Some("snippet_only"))
        .collect();
    assert!(
        !degraded.is_empty(),
        "(5) no item reported snippet_only: {full:?}"
    );
    for it in &degraded {
        assert!(
            !on_disk.contains_key(&it.item_id),
            "(5) {} has a concept",
            it.item_id
        );
    }
    println!("(5) {} snippet_only item(s), no concept", degraded.len());
}

/// Outcomes (6) to (8) over the Drive fixtures.
fn docs_outcomes(l: &Lane) {
    // (6) criterion 94: the recursive listing reaches the subfolder.
    let flat_root = l.bundle("docs-flat");
    import_run(l, GDOCS, &flat_root, &l.docs(false));
    let flat = concepts(&flat_root, "docs", DOCS_STORE).len();
    let root = l.bundle("docs");
    let r = import_run(l, GDOCS, &root, &l.docs(true));
    let on_disk = concepts(&root, "docs", DOCS_STORE);
    assert!(r.pagination_exhausted, "(6) {r:?}");
    assert!(
        on_disk.len() > flat,
        "(6) recursive {} vs flat {flat}: the subfolder's Doc is missing",
        on_disk.len()
    );
    for it in r.items.iter().filter(|i| i.read == "read") {
        assert!(
            on_disk.contains_key(&it.item_id),
            "(6) Doc {} has no concept",
            it.item_id
        );
    }
    println!(
        "(6) {} Doc concept(s), {flat} without recursion",
        on_disk.len()
    );

    // (7) criterion 91, live reading: the PDF is skipped, no status named.
    let skipped: Vec<_> = r.items.iter().filter(|i| i.read == "skipped").collect();
    assert!(!skipped.is_empty(), "(7) nothing skipped: {r:?}");
    for it in &skipped {
        assert!(
            !on_disk.contains_key(&it.item_id),
            "(7) {} has a concept",
            it.item_id
        );
    }
    println!("(7) {} skipped item(s)", skipped.len());

    // (8) criterion 91: the rich Doc keeps structure; the image fails nothing.
    assert_eq!(r.failed, 0, "(8) an item failed: {r:?}");
    let rich = on_disk.values().map(|rel| body(&root, rel)).find(|b| {
        let lines: Vec<&str> = b.lines().map(str::trim_start).collect();
        lines.iter().any(|x| x.starts_with('#'))
            && lines
                .iter()
                .any(|x| x.starts_with("- ") || x.starts_with("* ") || x.starts_with("1. "))
            && lines
                .iter()
                .any(|x| x.starts_with('|') && x.contains("---"))
            && b.contains("](http")
    });
    assert!(
        rich.is_some(),
        "(8) no Doc keeps a heading, list, table and link"
    );
    println!("(8) rich Doc keeps heading, list, table, link");
}

/// Why: criterion 148 — the lane is off unless `OKF_LIVE_PROFILE` is set,
/// and then runs against the owner's real account.
/// What: unset or empty, prints the skip and passes. Set, requires
/// `OKF_LIVE_DRIVE_FOLDER` and `HOME`, and runs [`run_lane`]; a pre-check
/// refusal or a failed outcome fails the test.
/// Test: this test (live in the owner channel only; skipped elsewhere).
#[test]
fn live_lane_against_the_owner_account() {
    let Some(profile) = std::env::var(PROFILE_VAR)
        .ok()
        .filter(|p| !p.trim().is_empty())
    else {
        println!(
            "skipped: {PROFILE_VAR} is not set, so the S13 live lane (criterion 148) did not run"
        );
        return;
    };
    let folder = std::env::var(FOLDER_VAR).unwrap_or_else(|_| {
        panic!("{PROFILE_VAR} is set but {FOLDER_VAR} (the fixture folder ID) is not")
    });
    let home = std::env::var_os("HOME").expect("HOME must hold ~/.gworkspace-mcp/tokens.json");
    let mut lane = Lane::new(Path::new(&home), profile.trim(), folder.trim(), None);
    if let Ok(q) = std::env::var(QUERY_VAR) {
        lane.query = q;
    }
    if let Err(why) = run_lane(&lane) {
        panic!("live lane refused for profile {profile}: {why}");
    }
}

// ---- offline: the pre-check and the lane steps against the fake --------

const WRITE_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/gmail.settings.basic",
    "https://mail.google.com/",
    "https://www.googleapis.com/auth/drive",
    "https://www.googleapis.com/auth/drive.file",
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/presentations",
    "https://www.googleapis.com/auth/tasks",
];

fn opts(fake: &FakeGoogle) -> Options {
    Options {
        api_base: Some(fake.url.clone()),
        config: None,
    }
}

fn tokeninfo_calls(fake: &FakeGoogle) -> usize {
    fake.seen()
        .iter()
        .filter(|s| s.path == "/oauth2.googleapis.com/tokeninfo")
        .count()
}

/// Why: the read-only preset is what the lane needs; it must pass.
/// What: tokeninfo reports exactly the preset; the pre-check returns it
/// and asked tokeninfo once, with the token in the form body.
/// Test: this test.
#[test]
fn precheck_passes_the_readonly_preset() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("ro")));
    fake.with(|st| st.tokeninfo_scope = Some(READONLY_SCOPES.join(" ")));
    assert_eq!(
        precheck(&h.home, &opts(&fake), "ro").unwrap(),
        READONLY_SCOPES
    );
    assert_eq!(tokeninfo_calls(&fake), 1);
    let seen = fake.seen();
    assert!(seen[0].query.contains_key("access_token"), "{seen:?}");
}

/// Why: the fail-open check — a write-capable token must be refused, and
/// the refusal must name the scope.
/// What: each write scope next to the preset is an `Err` naming it.
/// Test: this test.
#[test]
fn precheck_refuses_every_write_scope() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("ro")));
    for w in WRITE_SCOPES {
        fake.with(|st| st.tokeninfo_scope = Some(format!("{} {w}", READONLY_SCOPES.join(" "))));
        let e = precheck(&h.home, &opts(&fake), "ro").expect_err(w);
        assert!(e.contains(w), "{w}: {e}");
    }
}

/// Why: no verdict is not a pass, and no profile is defaulted.
/// What: a missing `scope` field, a tokeninfo error and an unknown profile
/// (with a default profile present) are each an `Err`; the unknown profile
/// never reaches tokeninfo.
/// Test: this test.
#[test]
fn precheck_fails_closed_without_a_verdict() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("ro")));
    assert!(
        precheck(&h.home, &opts(&fake), "ro").is_err(),
        "no scope field"
    );
    fake.with(|st| {
        st.tokeninfo_scope = Some(READONLY_SCOPES.join(" "));
        st.fail.push(("tokeninfo".into(), 400));
    });
    assert!(
        precheck(&h.home, &opts(&fake), "ro").is_err(),
        "tokeninfo 400"
    );
    fake.with(|st| st.fail.clear());
    fake.clear_seen();
    let e = precheck(&h.home, &opts(&fake), "other").unwrap_err();
    assert!(e.contains("other"), "{e}");
    assert_eq!(
        tokeninfo_calls(&fake),
        0,
        "no fallback to the default profile"
    );
}

/// Why: the pre-check must stop the lane before any import, never warn and
/// continue. This fails if `run_lane` skips or softens the pre-check.
/// What: a token with `gmail.modify`; `run_lane` returns `Err` naming it,
/// the fake sees only the tokeninfo call, and no bundle holds a concept.
/// Test: this test.
#[test]
fn a_refused_precheck_runs_no_import() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("ro")));
    fixtures(&fake);
    let w = "https://www.googleapis.com/auth/gmail.modify";
    fake.with(|st| st.tokeninfo_scope = Some(format!("{} {w}", READONLY_SCOPES.join(" "))));
    let lane = Lane::new(&h.home, "ro", "F", Some(&fake.url));
    let e = run_lane(&lane).expect_err("a write scope must refuse the lane");
    assert!(e.contains(w) && e.contains("no import"), "{e}");
    let seen = fake.seen();
    assert!(
        seen.iter()
            .all(|s| s.path == "/oauth2.googleapis.com/tokeninfo"),
        "a Google data call ran after the refusal: {seen:?}"
    );
    assert!(tree(lane.scratch.path()).is_empty(), "a bundle was written");
}

/// The S13 test-account fixtures, offline: SENT mail (a two-message
/// thread, a same-day same-subject pair, a quoted reply, a message with an
/// attachment, a message with no text part, one inbound message) and a
/// Drive folder (two Docs, one rich, a PDF, and a subfolder with a Doc).
fn fixtures(fake: &FakeGoogle) {
    let rich = "# Plan\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
                [site](https://example.com)\n\n![chart][image1]\n\n\
                [image1]: <data:image/png;base64,AAAA>\n";
    fake.with(|st| {
        st.page_cap = Some(2);
        st.messages
            .push(message("t-a", "th", JAN1, "Thread", "First.", true));
        st.messages.push(message(
            "t-b",
            "th",
            JAN1 + DAY,
            "Re: Thread",
            "Second.",
            true,
        ));
        st.messages
            .push(message("p-1", "tp1", JAN1 + 2 * DAY, "Pair", "One.", true));
        st.messages.push(message(
            "p-2",
            "tp2",
            JAN1 + 2 * DAY + 60_000,
            "Pair",
            "Two.",
            true,
        ));
        st.messages.push(message(
            "q-1",
            "tq",
            JAN1 + 3 * DAY,
            "Re: plan",
            "Agreed.\n\n> On Monday you wrote:\n> the plan",
            true,
        ));
        let mut att = message("a-1", "ta", JAN1 + 4 * DAY, "Deck", "Attached.", true);
        att["payload"]["parts"].as_array_mut().unwrap().push(json!({
            "mimeType": "application/pdf", "filename": "deck.pdf",
            "body": {"attachmentId": "A1", "size": 2048}}));
        st.messages.push(att);
        let mut bare = message("s-1", "ts", JAN1 + 5 * DAY, "Photo", "x", true);
        bare["payload"]["parts"] =
            json!([{"mimeType": "image/png", "filename": "a.png", "body": {"size": 5}}]);
        st.messages.push(bare);
        st.messages
            .push(message("in-1", "ti", JAN1, "Inbound", "Hello.", false));

        st.files.push(doc("d-1", "F", 1));
        st.files.push(doc("d-rich", "F", 1));
        st.files.push(
            json!({"id": "pdf-1", "name": "Spec.pdf", "mimeType": "application/pdf",
            "parents": ["F"], "version": "1", "modifiedTime": "2026-02-01T00:00:00Z",
            "createdTime": "2026-01-01T00:00:00Z", "trashed": false}),
        );
        st.files.push(folder("S", "F"));
        st.files.push(doc("d-sub", "S", 1));
        for (id, text) in [
            ("d-1", "Doc one.\n"),
            ("d-rich", rich),
            ("d-sub", "Sub doc.\n"),
        ] {
            st.exports.insert(
                (id.into(), "text/markdown".into()),
                text.as_bytes().to_vec(),
            );
        }
    });
}

/// Why: the steps cannot run live here (the egress guard blocks Gmail), so
/// they run against fixtures shaped like the test account's, through the
/// real adapters and the same `run_lane`.
/// What: a read-only token; `run_lane` passes all eight outcomes, reads no
/// attachment and no raw MIME, and never fetches the inbound message.
/// Test: this test.
#[test]
fn lane_steps_pass_against_the_fixture_fake() {
    let (fake, h) = (FakeGoogle::start(), Home::new(Some("ro")));
    fixtures(&fake);
    fake.with(|st| st.tokeninfo_scope = Some(READONLY_SCOPES.join(" ")));
    let lane = Lane::new(&h.home, "ro", "F", Some(&fake.url));
    run_lane(&lane).unwrap();
    for seen in fake.seen() {
        assert!(!seen.path.contains("/attachments/"), "{seen:?}");
        assert!(!seen.path.ends_with("/in-1"), "inbound fetched: {seen:?}");
        assert_ne!(seen.query.get("format").map(String::as_str), Some("raw"));
    }
}
