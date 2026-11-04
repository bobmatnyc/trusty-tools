//! SPEC-OKF-01 §6.2 criteria 71-82, 84, 136-138 and 144, and the import
//! halves of 116/117, for slice S8 of #10746 (document-tree import).
//!
//! Why: an import must leave its source unchanged, mirror the hierarchy
//! under deterministic IDs, refuse collisions and hand edits, converge on
//! re-import, tombstone only on a deletion-safe listing, quarantine every
//! protected source key, and report every item once with a reason.
//! What: each test builds a source tree and a bundle in sibling temporary
//! directories and runs `plan_import` and `import` through `DocTreeSource`.
//! Test: `cargo test -p trusty-okf --test import_criteria`.

mod s8_support;

use std::cell::Cell;

use s8_support::{Fx, concepts_snapshot, frontmatter, id, instant, now, outcome, snapshot};
use serde_yaml::Value;
use trusty_okf::OkfError;
use trusty_okf::build::docstore::{DEFAULT_CHUNK_CHARS, scan};
use trusty_okf::build::ingest::ingest_items;
use trusty_okf::build::policy::DocStorePolicy;
use trusty_okf::build::rebuild::rebuild;
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::bundle::{Bundle, VerificationState};
use trusty_okf::extract::{ExtractError, Extracted, Extractor, NoWorker, TypeHint};
use trusty_okf::format::{Actor, Verification, content_hash};
use trusty_okf::import::{Action, ImportSource, Listing, exit_code, plan_import};
use trusty_okf::profile::Profile;

/// A test-double worker: extracts any type as its UTF-8 text, counts calls.
#[derive(Default)]
struct FakeWorker {
    calls: Cell<usize>,
}

impl Extractor for FakeWorker {
    fn id(&self) -> &str {
        "fake-worker"
    }
    fn version(&self) -> &str {
        "1"
    }
    fn extract(&self, bytes: &[u8], _hint: &TypeHint) -> Result<Extracted, ExtractError> {
        self.calls.set(self.calls.get() + 1);
        Ok(Extracted {
            text: String::from_utf8_lossy(bytes).into_owned(),
            ..Extracted::default()
        })
    }
}

fn sha8(s: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(s.as_bytes())[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Why: criterion 71 — the source tree stays unchanged: no byte, no entry.
/// What: a tree with Markdown, text, a binary, a hidden directory and a
/// link; snapshots of every file hash and directory listing before and
/// after a plan and a run.
/// Test: self-contained.
#[test]
fn c71_import_leaves_the_source_tree_unchanged() {
    let fx = Fx::new(&[
        ("a.md", "---\ntitle: A\n---\nAlpha.\n"),
        ("notes/b.txt", "Beta.\n"),
        ("c.pdf", "%PDF-1.4 binary"),
        (".git/config", "[core]\n"),
    ]);
    std::os::unix::fs::symlink(fx.src.join("a.md"), fx.src.join("link.md")).unwrap();
    let before = snapshot(&fx.src);
    fx.plan(&fx.spec()).unwrap();
    let r = fx.import();
    assert!(r.created >= 2, "{r:?}");
    assert_eq!(snapshot(&fx.src), before);
}

/// Why: criterion 72 — a plan writes zero bytes to the bundle and the
/// source, and lists every item with its ID, action and extractor.
/// What: snapshots of both trees around `plan_import`, then the plan's
/// entries against the listing.
/// Test: self-contained.
#[test]
fn c72_plan_writes_nothing_and_lists_every_item() {
    let fx = Fx::new(&[
        ("a.md", "Alpha.\n"),
        ("b.txt", "Beta.\n"),
        ("c.pdf", "%PDF"),
    ]);
    let (src0, root0) = (snapshot(&fx.src), snapshot(&fx.root));
    let plan = fx.plan(&fx.spec()).unwrap();
    assert_eq!((snapshot(&fx.src), snapshot(&fx.root)), (src0, root0));
    assert_eq!(plan.listed, 3);
    assert_eq!(plan.items.len(), 3);
    for p in &plan.items {
        assert!(p.id.is_some(), "{p:?}");
        assert!(p.extractor.is_some(), "{p:?}");
        assert_eq!(p.action, Action::Create, "{p:?}");
    }
    let pdf = plan.items.iter().find(|p| p.item_id == "c.pdf").unwrap();
    assert_eq!(pdf.extractor.as_deref(), Some("worker"));
}

/// Why: criterion 73 — the ID tree mirrors the source hierarchy; `a.md` is
/// `a` and `a.pdf` is `a.pdf`, written as `a.pdf.md`.
/// What: a nested tree, a test-double worker for the PDF.
/// Test: self-contained.
#[test]
fn c73_ids_mirror_the_source_hierarchy() {
    let fx = Fx::new(&[
        ("a.md", "Alpha.\n"),
        ("a.pdf", "pdf text"),
        ("x/y/z.md", "Zed.\n"),
    ]);
    let r = fx
        .import_with(&fx.spec(), &FakeWorker::default(), now())
        .unwrap();
    assert_eq!(r.created, 3, "{r:?}");
    for f in ["a.md", "a.pdf.md", "x/y/z.md"] {
        assert!(fx.root.join(f).is_file(), "{f}");
    }
    let fm = frontmatter(&fx.root, "a.pdf.md");
    assert_eq!(fm.get("extracted_by"), Some(&Value::from("fake-worker@1")));
}

/// Why: criterion 74 — names §2.11 refuses map to normalized IDs with the
/// `~<h8>` suffix, the same on two machines.
/// What: the three example names imported from two trees at different
/// absolute paths.
/// Test: self-contained.
#[test]
fn c74_refused_names_normalize_with_a_stable_suffix() {
    let names = [("index.md", "x"), ("C:x.md", "y"), ("_x/y.md", "z")];
    let want = [
        format!("index~{}.md", sha8("index.md")),
        format!("C-x~{}.md", sha8("C:x.md")),
        format!("-x~{}/y.md", sha8("_x/y.md")),
    ];
    let (one, two) = (Fx::new(&names), Fx::new(&names));
    assert_ne!(one.src, two.src);
    for fx in [&one, &two] {
        let r = fx.import();
        assert_eq!(r.created, 3, "{r:?}");
        for w in &want {
            assert!(fx.root.join(w).is_file(), "{w} missing");
        }
    }
}

/// Why: criterion 75 — two colliding source paths are both refused with
/// `collision` and neither is written; a ledger-pinned ID keeps its concept
/// when a new colliding file appears.
/// What: `Notes/a.md` and `notes/a.md` (one fold key); then `x/a.md` alone,
/// and `X/a.md` added.
/// Test: self-contained.
#[test]
fn c75_colliding_sources_are_refused_and_pins_hold() {
    let fx = Fx::new(&[("Notes/a.md", "One.\n"), ("notes/a.md", "Two.\n")]);
    let r = fx.import();
    for item in ["Notes/a.md", "notes/a.md"] {
        assert_eq!(outcome(&r, item).1, "failed");
        assert_eq!(outcome(&r, item).2.as_deref(), Some("collision"));
    }
    assert!(!fx.root.join("Notes").exists() && !fx.root.join("notes").exists());
    assert_eq!(r.exit_code, 3);

    let fx = Fx::new(&[("x/a.md", "Pinned.\n")]);
    fx.import();
    let pinned = fx.read("x/a.md");
    fx.write("X/a.md", "Usurper.\n");
    let r = fx.import();
    assert_eq!(outcome(&r, "X/a.md").2.as_deref(), Some("collision"));
    assert_eq!(fx.read("x/a.md"), pinned);
}

/// Why: criterion 76 — provenance: `sources[]` with the source-relative
/// path, the four `source_*` keys, and no absolute path in the bundle (the
/// registry row holds the root).
/// What: one nested file; every bundle file except the registry is scanned
/// for the source root.
/// Test: self-contained.
#[test]
fn c76_imported_concepts_carry_relative_provenance() {
    let fx = Fx::new(&[("notes/a.md", "Alpha.\n")]);
    fx.import();
    let fm = frontmatter(&fx.root, "notes/a.md");
    let sources = fm.get("sources").unwrap().as_sequence().unwrap();
    assert_eq!(sources[0].get("resource"), Some(&Value::from("notes/a.md")));
    assert_eq!(fm.get("source_id"), Some(&Value::from(s8_support::STORE)));
    assert_eq!(fm.get("source_kind"), Some(&Value::from("docstore")));
    assert_eq!(fm.get("source_item_id"), Some(&Value::from("notes/a.md")));
    assert!(
        fm.get("source_timestamp")
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with('Z'))
    );
    let abs = fx.src.to_string_lossy().into_owned();
    for e in walkdir::WalkDir::new(&fx.root) {
        let e = e.unwrap();
        if e.file_type().is_file() && !e.path().ends_with("_sources/registry.toml") {
            let text = String::from_utf8_lossy(&std::fs::read(e.path()).unwrap()).into_owned();
            assert!(!text.contains(&abs), "{} holds {abs}", e.path().display());
        }
    }
}

/// Why: criterion 77 and brief point 2 — plan, import, then re-import
/// writes zero bytes and plans every item unchanged; a changed source file
/// updates its concept, keeps the editorial keys and moves verification to
/// history.
/// What: a full-tree byte snapshot around the second run; then a verify, a
/// deprecate and a source edit.
/// Test: self-contained.
#[test]
fn c77_second_import_writes_zero_bytes_and_a_change_updates() {
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("b.txt", "Beta.\n")]);
    fx.import();
    let before = snapshot(&fx.root);
    let plan = fx.plan(&fx.spec()).unwrap();
    assert!(
        plan.items.iter().all(|p| p.action == Action::Unchanged),
        "{plan:?}"
    );
    let r = fx.import();
    assert_eq!((r.written, r.unchanged, r.exit_code), (0, 2, 0), "{r:?}");
    assert_eq!(snapshot(&fx.root), before, "a repeat import wrote bytes");

    let mut b = Bundle::open(&fx.root).unwrap().0;
    let a = id("a");
    let seen = content_hash(&b.get(&a).unwrap());
    let v = Verification {
        by: Actor::Process("rev".into()),
        at: now(),
        stale_after: None,
    };
    b.verify(&a, v, &seen).unwrap();
    b.deprecate(&a, "old", Actor::Process("ed".into()), now())
        .unwrap();
    fx.write("a.md", "Alpha changed.\n");
    let r = fx
        .import_with(&fx.spec(), &NoWorker, instant("2026-11-04T13:00:00Z"))
        .unwrap();
    assert_eq!(outcome(&r, "a.md").1, "updated", "{r:?}");
    assert_eq!(r.verification_invalidated, 1);
    let fm = frontmatter(&fx.root, "a.md");
    assert_eq!(fm.get("status"), Some(&Value::from("deprecated")));
    assert!(fm.get("verified").is_none());
    let b = Bundle::open(&fx.root).unwrap().0;
    assert!(
        matches!(b.verification_state(&a).unwrap(), VerificationState::Unverified { history } if history >= 1)
    );
}

/// Why: criterion 78 — a hand-edited imported concept is refused with a
/// conflict on re-import, reported, and keeps its bytes.
/// What: an import, a hand edit of the concept, a source change, a re-run.
/// Test: self-contained.
#[test]
fn c78_hand_edited_concept_is_refused_on_reimport() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    fx.import();
    let edited = format!("{}Hand note.\n", fx.read("a.md"));
    std::fs::write(fx.root.join("a.md"), &edited).unwrap();
    fx.write("a.md", "Alpha two.\n");
    assert_eq!(
        fx.plan(&fx.spec()).unwrap().items[0].action,
        Action::Conflict
    );
    let r = fx.import();
    assert_eq!(
        outcome(&r, "a.md"),
        ("read".into(), "failed".into(), Some("hand_edit".into()))
    );
    assert_eq!(fx.read("a.md"), edited);
    assert_eq!(r.exit_code, 3);
}

/// Why: criterion 79 — a removed source file is tombstoned only when the
/// run is deletion-safe; otherwise it is reported missing.
/// What: removal with `tombstone_deleted` off, with the ceiling reached,
/// then with neither.
/// Test: self-contained.
#[test]
fn c79_vanished_files_tombstone_only_when_deletion_safe() {
    let fx = Fx::new(&[
        ("a.md", "Alpha.\n"),
        ("b.md", "Beta.\n"),
        ("c.md", "Gamma.\n"),
    ]);
    fx.import();
    let b0 = fx.read("b.md");
    fx.remove("b.md");
    let mut off = fx.spec();
    off.store.tombstone_deleted = false;
    let r = fx.import_with(&off, &NoWorker, now()).unwrap();
    assert_eq!(
        (r.deletion_safe, outcome(&r, "b.md").1.as_str()),
        (false, "missing")
    );
    assert_eq!(fx.read("b.md"), b0);
    let mut capped = fx.spec();
    capped.max_items = 2;
    let r = fx.import_with(&capped, &NoWorker, now()).unwrap();
    assert!(!r.pagination_exhausted && !r.deletion_safe);
    assert_eq!(outcome(&r, "b.md").1, "missing");
    assert_eq!(fx.read("b.md"), b0);
    let r = fx.import();
    assert!(r.deletion_safe);
    assert_eq!(outcome(&r, "b.md").1, "tombstoned");
    assert_eq!(
        frontmatter(&fx.root, "b.md").get("source_status"),
        Some(&Value::from("deleted"))
    );
}

/// Why: criterion 80 — every skipped item carries one §2.14 rule 8 reason,
/// and the report's item count equals the listed count.
/// What: a tree that produces every reason a document tree can.
/// Test: self-contained.
#[test]
fn c80_every_item_is_reported_once_with_a_reason() {
    let fx = Fx::new(&[
        ("ok.md", "Fine.\n"),
        ("skip/me.md", "Excluded.\n"),
        (".hidden/x.md", "Hidden.\n"),
        ("c.pdf", "%PDF"),
        ("n.txt", "a\0b"),
    ]);
    s8_support::write_bytes(&fx.src, "big.txt", &vec![b'a'; 8 * 1024 * 1024 + 1]);
    std::os::unix::fs::symlink(fx.src.join("ok.md"), fx.src.join("l.md")).unwrap();
    let mut spec = fx.spec();
    spec.exclude = vec!["skip/**".into()];
    let plan = fx.plan(&spec).unwrap();
    let r = fx.import_with(&spec, &NoWorker, now()).unwrap();
    assert_eq!(r.items.len(), plan.items.len());
    assert_eq!(r.items.len(), r.listed);
    let reason = |i: &str| outcome(&r, i).2.unwrap_or_default();
    assert_eq!(reason("skip/me.md"), "excluded");
    assert_eq!(reason(".hidden"), "hidden");
    assert_eq!(reason("l.md"), "symlink");
    assert_eq!(reason("big.txt"), "too_large");
    assert_eq!(reason("n.txt"), "binary_no_extractor");
    assert_eq!(reason("c.pdf"), "worker_absent");
    let rule8 = [
        "excluded",
        "hidden",
        "symlink",
        "too_large",
        "binary_no_extractor",
        "unsupported_format",
        "worker_absent",
        "worker_incompatible",
        "policy_refused",
        "sandbox_required",
        "collision",
    ];
    for i in r.items.iter().filter(|i| i.read == "skipped") {
        assert!(rule8.contains(&i.reason.as_deref().unwrap_or("")), "{i:?}");
    }
    assert_eq!(r.skipped, 6);
    assert_eq!(r.exit_code, 3);
}

/// Why: criterion 81 — a link under the source root is reported `symlink`
/// and never read, also when it points inside the root.
/// What: one link inside, one outside; the source's fetch of a link fails.
/// Test: self-contained.
#[test]
fn c81_symlinks_are_reported_and_never_read() {
    let fx = Fx::new(&[("real.md", "Real.\n")]);
    let outside = fx.tmp.path().join("secret.md");
    std::fs::write(&outside, "secret phrase\n").unwrap();
    std::os::unix::fs::symlink(fx.src.join("real.md"), fx.src.join("inside.md")).unwrap();
    std::os::unix::fs::symlink(&outside, fx.src.join("outside.md")).unwrap();
    std::os::unix::fs::symlink(fx.src.join("real.md"), fx.src.join("dir")).unwrap();
    let r = fx.import();
    for l in ["inside.md", "outside.md", "dir"] {
        assert_eq!(outcome(&r, l).2.as_deref(), Some("symlink"), "{l}");
        assert!(!fx.root.join(format!("{l}.md")).exists());
    }
    let err = fx.source().fetch("outside.md").unwrap_err();
    assert_eq!(err.code, "symlink");
}

/// Why: criterion 82 — overlapping roots are refused with `SourceOverlap`
/// (exit 2), before anything is written.
/// What: a bundle inside the source, and a source inside the bundle.
/// Test: self-contained.
#[test]
fn c82_overlapping_roots_are_refused() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    let inner = fx.src.join("kb");
    std::fs::create_dir_all(&inner).unwrap();
    let mut inside = Fx::new(&[]);
    inside.src = fx.src.clone();
    inside.root = inner;
    let before = snapshot(&fx.src);
    let got = inside.plan(&inside.spec());
    assert!(
        matches!(got, Err(OkfError::SourceOverlap { .. })),
        "{got:?}"
    );
    assert_eq!(
        exit_code(&got.map(|_| trusty_okf::import::ImportReport::default())),
        2
    );
    assert_eq!(snapshot(&fx.src), before);
    let mut reverse = Fx::new(&[]);
    reverse.src = reverse.root.join("docs");
    std::fs::create_dir_all(&reverse.src).unwrap();
    std::fs::write(reverse.src.join("a.md"), "A.\n").unwrap();
    let before = snapshot(&reverse.root);
    let got = reverse.plan(&reverse.spec());
    assert!(
        matches!(got, Err(OkfError::SourceOverlap { .. })),
        "{got:?}"
    );
    assert_eq!(snapshot(&reverse.root), before);
}

/// Why: criteria 84 and 116/117 (import half, brief point 3) — no imported
/// concept has `verified`; `approved` stays an extension key; every
/// protected source key is quarantined under `source_fields` with a
/// `quarantined_key` finding; a planted verification-record hash never
/// verifies, after import and after re-import.
/// What: a source file whose frontmatter plants protected keys; the
/// re-import plants the content hash of the revision the first import
/// wrote (the planted value is itself content, so no revision can carry
/// its own hash).
/// Test: self-contained.
#[test]
fn c84_protected_source_keys_are_quarantined_and_never_verify() {
    let planted = "---\ntitle: A\napproved: yes\nverified:\n- by: human:mallory\n  at: 2026-01-01T00:00:00Z\nstatus: deprecated\nsource_status: deleted\ntrust: UserAuthored\nverification_hash: PLACEHOLDER\n---\nBody.\n";
    let fx = Fx::new(&[("a.md", planted)]);
    let r = fx.import();
    let a = id("a");
    let hash = content_hash(&Bundle::open(&fx.root).unwrap().0.get(&a).unwrap());
    fx.write("a.md", &planted.replace("PLACEHOLDER", hash.as_str()));
    let r2 = fx.import();
    for (report, step) in [(&r, "import"), (&r2, "re-import")] {
        let keys: Vec<&str> = report
            .findings
            .iter()
            .filter(|f| f.kind == "quarantined_key")
            .map(|f| f.detail.as_str())
            .collect();
        for k in [
            "verified",
            "status",
            "source_status",
            "trust",
            "verification_hash",
        ] {
            assert!(keys.contains(&k), "{step}: {k} not quarantined: {keys:?}");
        }
    }
    let fm = frontmatter(&fx.root, "a.md");
    assert!(fm.get("verified").is_none() && fm.get("status").is_none());
    assert_eq!(fm.get("approved"), Some(&Value::from("yes")));
    let held = fm.get("source_fields").unwrap();
    assert!(held.get("verified").is_some() && held.get("verification_hash").is_some());
    assert_eq!(
        fm.get("generated").and_then(|g| g.get("by")),
        Some(&Value::from("process:trusty-okf-import"))
    );
    assert_eq!(fm.get("trust"), Some(&Value::from("untrusted-external")));
    let b = Bundle::open(&fx.root).unwrap().0;
    assert_eq!(
        b.verification_state(&a).unwrap(),
        VerificationState::Unverified { history: 0 }
    );
    assert!(b.verification_events(&a).unwrap().is_empty());
}

/// Why: criterion 136 — full-ID addressing: only the vanished
/// `notes/sub/a` is tombstoned, `notes/a` keeps its bytes, and only
/// `notes/sub/a` returns to Live when its source reappears.
/// What: both files from one store, a removal, then a restore.
/// Test: self-contained.
#[test]
fn c136_tombstone_and_reversal_address_the_pinned_concept() {
    let fx = Fx::new(&[("notes/sub/a.md", "Deep.\n"), ("notes/a.md", "Shallow.\n")]);
    fx.import();
    let shallow = fx.read("notes/a.md");
    fx.remove("notes/sub/a.md");
    let r = fx.import();
    assert_eq!(outcome(&r, "notes/sub/a.md").1, "tombstoned");
    assert_eq!(
        frontmatter(&fx.root, "notes/sub/a.md").get("source_status"),
        Some(&Value::from("deleted"))
    );
    assert_eq!(fx.read("notes/a.md"), shallow);
    fx.write("notes/sub/a.md", "Deep.\n");
    let r = fx.import();
    assert_eq!(outcome(&r, "notes/sub/a.md").1, "updated");
    assert!(
        frontmatter(&fx.root, "notes/sub/a.md")
            .get("source_status")
            .is_none()
    );
    assert_eq!(fx.read("notes/a.md"), shallow);
}

/// Why: criterion 137 — a 50,000-character file is one concept and one
/// ledger item with the whole text; a repeat writes zero bytes; its
/// removal tombstones that one concept and nothing else.
/// What: the large file beside a small one.
/// Test: self-contained.
#[test]
fn c137_large_file_is_one_concept() {
    let para = "word ".repeat(19);
    let text: String = (0..526).map(|i| format!("{i} {para}\n\n")).collect();
    assert!(text.chars().count() >= 50_000);
    let fx = Fx::new(&[("big.md", &text), ("small.md", "Small.\n")]);
    let r = fx.import();
    assert_eq!((r.created, r.failed, r.write_failed), (2, 0, 0), "{r:?}");
    assert!(fx.read("big.md").contains(text.trim()));
    let ledger = std::fs::read_to_string(fx.root.join("_sources/import-src.jsonl")).unwrap();
    assert_eq!(ledger.lines().filter(|l| l.contains("big.md")).count(), 1);
    let before = snapshot(&fx.root);
    fx.import();
    assert_eq!(snapshot(&fx.root), before);
    let small = fx.read("small.md");
    fx.remove("big.md");
    let r = fx.import();
    assert_eq!(r.tombstoned, 1);
    assert_eq!(fx.read("small.md"), small);
}

fn tree_store(dir: &std::path::Path) -> SourceSpec {
    let mut s = SourceSpec::new(
        "docs",
        Some("sources"),
        Locator::DocStore {
            path: dir.to_string_lossy().into_owned(),
            extensions: Vec::new(),
            recursive: true,
        },
        "2026-11-04T00:00:00Z",
    );
    s.tombstone_deleted = true;
    s
}

/// Why: criterion 138 — `build` and `rebuild` refuse every colliding item
/// and count none as written; a pinned concept keeps its bytes when a
/// colliding file appears; `import` maps the same four files to four IDs.
/// What: `a.md`, `a.txt`, `nested/c.md` and `nested-c.md` through the
/// doc-store scan and `ingest_items`, then `rebuild`, then `a.md` alone and
/// `a.txt` added; finally an import.
/// Test: self-contained.
#[test]
fn c138_colliding_items_are_refused_and_pins_hold() {
    let files = [
        ("a.md", "A md.\n"),
        ("a.txt", "A txt.\n"),
        ("nested/c.md", "C1.\n"),
        ("nested-c.md", "C2.\n"),
    ];
    let fx = Fx::new(&files);
    let policy = DocStorePolicy::new(vec![fx.src.clone()]);
    let items = scan(&fx.src, &[], true, DEFAULT_CHUNK_CHARS, &policy)
        .unwrap()
        .items;
    assert_eq!(items.len(), 4);
    let spec = tree_store(&fx.src);
    let profile = Profile::default_profile();
    let r = ingest_items(&fx.root, &profile, &spec, &items, true, now()).unwrap();
    assert_eq!(r.failed.len(), 4, "{r:?}");
    assert!(r.failed.iter().all(|f| f.reason == "collision"));
    assert_eq!((r.ingested, r.updated), (0, 0));
    assert!(!fx.root.join("sources").exists());
    let rb = rebuild(&fx.root, &profile, &[(spec.clone(), items.clone())], now()).unwrap();
    assert_eq!(
        rb.conflicts
            .iter()
            .filter(|c| c.reason == "collision")
            .count(),
        4,
        "{rb:?}"
    );
    assert!(rb.rebuilt.is_empty() && !fx.root.join("sources").exists());

    let one = Fx::new(&[("a.md", "A md.\n")]);
    let spec1 = tree_store(&one.src);
    let pol1 = DocStorePolicy::new(vec![one.src.clone()]);
    let first = scan(&one.src, &[], true, DEFAULT_CHUNK_CHARS, &pol1)
        .unwrap()
        .items;
    ingest_items(&one.root, &profile, &spec1, &first, true, now()).unwrap();
    let pinned = std::fs::read(one.root.join("sources/a.md")).unwrap();
    one.write("a.txt", "A txt.\n");
    let both = scan(&one.src, &[], true, DEFAULT_CHUNK_CHARS, &pol1)
        .unwrap()
        .items;
    let r = ingest_items(&one.root, &profile, &spec1, &both, true, now()).unwrap();
    assert_eq!(r.failed.len(), 1, "{r:?}");
    assert_eq!(r.failed[0].item_id, "a.txt");
    assert_eq!(
        std::fs::read(one.root.join("sources/a.md")).unwrap(),
        pinned
    );

    let r = fx.import();
    assert_eq!(r.created, 4, "{r:?}");
    assert!(r.items.iter().all(|i| i.reason.is_none()), "{r:?}");
}

/// A source whose store is a connector, listing one item.
struct ConnectorSource;

impl ImportSource for ConnectorSource {
    fn list(&mut self, _cursor: Option<&str>) -> Result<Listing, OkfError> {
        Ok(Listing {
            entries: Vec::new(),
            cursor: None,
            complete: true,
        })
    }
    fn fetch(&mut self, _item_id: &str) -> Result<Vec<u8>, ExtractError> {
        unreachable!("a refused run fetches nothing")
    }
}

/// Why: criterion 144 — with `child_processes: false`, a local tree imports
/// `a.md` and `b.txt`, skips `c.pdf` with `policy_refused`, and the worker
/// (the process-spawn hook) is never called; a `gmail` store is refused
/// with `policy_refused` before any write, and the library maps that
/// refusal to exit code 2.
/// What: a counting test-double worker; a connector store, refused at plan
/// and again at apply when the spec is flipped after planning.
/// Test: self-contained.
#[test]
fn c144_no_child_processes_skips_worker_items_and_refuses_connectors() {
    let fx = Fx::new(&[("a.md", "A.\n"), ("b.txt", "B.\n"), ("c.pdf", "%PDF")]);
    let mut spec = fx.spec();
    spec.child_processes = false;
    let worker = FakeWorker::default();
    let r = fx.import_with(&spec, &worker, now()).unwrap();
    assert_eq!(
        worker.calls.get(),
        0,
        "a child-process extractor was called"
    );
    assert_eq!(outcome(&r, "a.md").1, "created");
    assert_eq!(outcome(&r, "b.txt").1, "created");
    assert_eq!(
        outcome(&r, "c.pdf"),
        (
            "skipped".into(),
            "none".into(),
            Some("policy_refused".into())
        )
    );
    assert_eq!(r.exit_code, 3);

    let mail = Fx::new(&[]);
    let mut spec = mail.spec();
    spec.store.locator = Locator::Connector {
        kind: "gmail".into(),
        params: ConnectorParams::new(),
    };
    let before = snapshot(&mail.root);
    let b = Bundle::open(&mail.root).unwrap().0;
    let plan = plan_import(&b, &spec, &mut ConnectorSource).unwrap();
    spec.child_processes = false;
    let refused = plan_import(&b, &spec, &mut ConnectorSource);
    assert!(
        matches!(&refused, Err(OkfError::PolicyRefused { reason, .. }) if reason.starts_with("policy_refused")),
        "{refused:?}"
    );
    let mut flipped = plan.clone();
    flipped.spec.child_processes = false;
    let got = trusty_okf::import::import(
        &mut Bundle::open(&mail.root).unwrap().0,
        &flipped,
        &mut ConnectorSource,
        &worker,
        now(),
    );
    assert!(
        matches!(got, Err(OkfError::PolicyRefused { .. })),
        "{got:?}"
    );
    assert_eq!(exit_code(&got), 2);
    assert_eq!(snapshot(&mail.root), before, "a refused run wrote");
}

/// Why: §4.10 phase 2 — bytes that changed after the plan are not written
/// under the plan's decision (`stale_plan`), and nothing else is affected.
/// What: a plan, a source edit, then the apply of the old plan.
/// Test: self-contained.
#[test]
fn source_changed_after_the_plan_fails_stale_plan() {
    let fx = Fx::new(&[("a.md", "A.\n"), ("b.md", "B.\n")]);
    let plan = fx.plan(&fx.spec()).unwrap();
    fx.write("a.md", "A changed.\n");
    let mut b = Bundle::open(&fx.root).unwrap().0;
    let r = trusty_okf::import::import(&mut b, &plan, &mut fx.source(), &NoWorker, now()).unwrap();
    assert_eq!(
        outcome(&r, "a.md"),
        ("failed".into(), "none".into(), Some("stale_plan".into()))
    );
    assert_eq!(outcome(&r, "b.md").1, "created");
    assert!(!fx.root.join("a.md").exists());
    let _ = concepts_snapshot(&fx.root);
}
