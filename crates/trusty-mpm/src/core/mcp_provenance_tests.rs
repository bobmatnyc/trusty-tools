//! Tests for the `.mcp.json` provenance ledger.
//!
//! Why: this ledger is the sole evidence a quarantine acts on, so the tests
//! that matter most are the ones proving it says "I cannot tell" rather than
//! guessing — an unreadable ledger, an unreadable file, and a file with no
//! record must each classify as something the repair refuses.
//! Test: this file.

use super::*;

/// A framework root inside a fresh tempdir.
fn root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).unwrap();
    (tmp, root)
}

/// Write `content` to `<dir>/.mcp.json` and return the path.
fn write_mcp(dir: &Path, content: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(".mcp.json");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn ledger_path_is_under_the_framework_root() {
    let path = ledger_path(Path::new("/base/.trusty-mpm"));
    assert_eq!(
        path,
        Path::new("/base/.trusty-mpm/mcp-json-provenance.json")
    );
}

#[test]
fn load_reports_missing_when_absent() {
    let (_tmp, root) = root();
    assert!(matches!(load(&root), LedgerLoad::Missing));
}

#[test]
fn load_reports_unreadable_on_malformed_json() {
    // A truncated or hand-mangled ledger must NEVER read as an empty one:
    // "tm wrote nothing" and "I cannot tell what tm wrote" lead to different
    // repair decisions, and only one of them is safe.
    let (_tmp, root) = root();
    std::fs::write(ledger_path(&root), "{ not json").unwrap();
    let LedgerLoad::Unreadable(why) = load(&root) else {
        panic!("a malformed ledger must be Unreadable, never Missing or Loaded");
    };
    assert!(why.contains("malformed"), "the reason must say so: {why}");
}

#[test]
fn ledger_round_trips() {
    let (_tmp, root) = root();
    record_write(&root, Path::new("/a/.mcp.json"), "{}").unwrap();
    let LedgerLoad::Loaded(ledger) = load(&root) else {
        panic!("expected a loaded ledger");
    };
    assert_eq!(ledger.version, LEDGER_VERSION);
    assert!(ledger.written.contains_key("/a/.mcp.json"));
}

#[test]
fn record_then_classify_reports_unmodified() {
    let (tmp, root) = root();
    let content = r#"{"mcpServers":{}}"#;
    let path = write_mcp(&tmp.path().join("ws"), content);
    record_write(&root, &path, content).unwrap();
    assert_eq!(classify(&load(&root), &path), Provenance::TmWritten);
}

#[test]
fn classify_reports_edited_when_bytes_changed() {
    // The whole reason the record carries a checksum: an operator who edited
    // tm's file owns its current contents, and the repair must not treat it as
    // tm's own residue.
    let (tmp, root) = root();
    let path = write_mcp(&tmp.path().join("ws"), r#"{"mcpServers":{}}"#);
    record_write(&root, &path, r#"{"mcpServers":{}}"#).unwrap();
    std::fs::write(&path, r#"{"mcpServers":{"mine":{}}}"#).unwrap();
    assert_eq!(
        classify(&load(&root), &path),
        Provenance::TmWrittenThenEdited
    );
}

#[test]
fn classify_reports_unattributed_without_a_record() {
    // Every `.mcp.json` written before this ledger existed lands here — the
    // observed /private/tmp file included.
    let (tmp, root) = root();
    let path = write_mcp(&tmp.path().join("elsewhere"), r#"{"mcpServers":{}}"#);
    assert_eq!(classify(&load(&root), &path), Provenance::Unattributed);
}

#[test]
fn classify_reports_unattributed_when_the_ledger_is_missing() {
    let (tmp, _root) = root();
    let path = write_mcp(&tmp.path().join("ws"), "{}");
    assert_eq!(
        classify(&LedgerLoad::Missing, &path),
        Provenance::Unattributed
    );
}

#[test]
fn classify_reports_unknown_when_the_ledger_is_unreadable() {
    // Failure path: with no readable ledger tm knows nothing about any file,
    // and must say so rather than fall through to "not ours" (which reads the
    // same but is a claim) or "ours" (which would be catastrophic).
    let (tmp, _root) = root();
    let path = write_mcp(&tmp.path().join("ws"), "{}");
    let verdict = classify(&LedgerLoad::Unreadable("boom".to_string()), &path);
    let Provenance::Unknown(why) = verdict else {
        panic!("an unreadable ledger must yield Unknown, got {verdict:?}");
    };
    assert!(why.contains("unreadable"), "reason must say why: {why}");
}

#[test]
fn classify_reports_unknown_when_the_file_is_unreadable() {
    // Failure path: a record exists but the file cannot be re-read, so the
    // checksum cannot be verified. Trusting the record alone would quarantine
    // a file whose bytes might since have become somebody else's.
    let (tmp, root) = root();
    let path = tmp.path().join("gone").join(".mcp.json");
    record_write(&root, &path, "{}").unwrap();
    let verdict = classify(&load(&root), &path);
    let Provenance::Unknown(why) = verdict else {
        panic!("an unreadable file must yield Unknown, got {verdict:?}");
    };
    assert!(why.contains("checksum"), "reason must say why: {why}");
}

#[test]
fn record_is_idempotent_for_identical_content() {
    let (tmp, root) = root();
    let path = write_mcp(&tmp.path().join("ws"), "{}");
    record_write(&root, &path, "{}").unwrap();
    let first = std::fs::read_to_string(ledger_path(&root)).unwrap();
    record_write(&root, &path, "{}").unwrap();
    let second = std::fs::read_to_string(ledger_path(&root)).unwrap();
    // Only `written_at` may differ; the entry count and checksum must not grow.
    let a: McpProvenanceLedger = serde_json::from_str(&first).unwrap();
    let b: McpProvenanceLedger = serde_json::from_str(&second).unwrap();
    assert_eq!(a.written.len(), b.written.len());
    assert_eq!(
        a.written.values().next().unwrap().checksum,
        b.written.values().next().unwrap().checksum
    );
}

#[test]
fn record_refuses_to_clobber_an_unreadable_ledger() {
    // Failure path: overwriting a corrupt ledger with a fresh one would drop
    // every prior attribution, silently downgrading future repairs to
    // refusals. Better to fail the (non-fatal) record.
    let (tmp, root) = root();
    std::fs::write(ledger_path(&root), "{ not json").unwrap();
    let path = write_mcp(&tmp.path().join("ws"), "{}");
    let err = record_write(&root, &path, "{}").expect_err("must not clobber");
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "the error must name the refusal: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(ledger_path(&root)).unwrap(),
        "{ not json",
        "the corrupt ledger must be left exactly as found"
    );
}

#[test]
fn record_preserves_other_entries() {
    let (_tmp, root) = root();
    record_write(&root, Path::new("/a/.mcp.json"), "{}").unwrap();
    record_write(&root, Path::new("/b/.mcp.json"), "{}").unwrap();
    let LedgerLoad::Loaded(ledger) = load(&root) else {
        panic!("expected a loaded ledger");
    };
    assert_eq!(ledger.written.len(), 2);
}

#[test]
fn forget_releases_the_claim() {
    let (_tmp, root) = root();
    record_write(&root, Path::new("/a/.mcp.json"), "{}").unwrap();
    record_write(&root, Path::new("/b/.mcp.json"), "{}").unwrap();
    forget(&root, Path::new("/a/.mcp.json"));
    let LedgerLoad::Loaded(ledger) = load(&root) else {
        panic!("expected a loaded ledger");
    };
    assert!(!ledger.written.contains_key("/a/.mcp.json"));
    assert!(
        ledger.written.contains_key("/b/.mcp.json"),
        "forgetting one claim must not drop the others"
    );
}

#[test]
fn forget_is_a_noop_without_a_ledger() {
    // Runs AFTER the filesystem change, so it must never turn a completed
    // repair into an error.
    let (_tmp, root) = root();
    forget(&root, Path::new("/a/.mcp.json"));
    assert!(matches!(load(&root), LedgerLoad::Missing));
}

#[test]
fn absolute_key_is_stable_for_a_relative_path() {
    let key = absolute_key(Path::new("rel/.mcp.json"));
    assert!(
        Path::new(&key).is_absolute(),
        "a relative path must never become a ledger key: {key}"
    );
}

#[test]
fn record_serialises_concurrent_writers() {
    // Two managed launches can prepare sessions at the same time. Without the
    // ledger lock the second load-modify-save publishes a document missing the
    // first's entry, and the file that entry described becomes unattributable
    // — a lost-update race that shows up later as an unexplained refusal.
    let (_tmp, root) = root();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let root = root.clone();
            std::thread::spawn(move || {
                record_write(&root, &PathBuf::from(format!("/p{i}/.mcp.json")), "{}").unwrap();
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let LedgerLoad::Loaded(ledger) = load(&root) else {
        panic!("expected a loaded ledger");
    };
    assert_eq!(
        ledger.written.len(),
        8,
        "every concurrent write must survive: {:?}",
        ledger.written.keys().collect::<Vec<_>>()
    );
}

#[cfg(unix)]
#[test]
fn absolute_key_collapses_symlinked_parents() {
    // The macOS `/tmp` -> `/private/tmp` case, reproduced. A write recorded via
    // one spelling must be found by a scan arriving via the other, or the file
    // is unattributable and the repair refuses a file it actually wrote.
    let (tmp, _root) = root();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(
        absolute_key(&link.join(".mcp.json")),
        absolute_key(&real.join(".mcp.json")),
        "two spellings of one directory must produce one ledger key"
    );
}

#[test]
fn absolute_key_survives_a_missing_file() {
    // `forget` runs after the file is gone; a key that needed the FILE to exist
    // would fail exactly when the claim must be released.
    let (tmp, _root) = root();
    let key = absolute_key(&tmp.path().join(".mcp.json"));
    assert!(key.ends_with(".mcp.json"), "{key}");
}
