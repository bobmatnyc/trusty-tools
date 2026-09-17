//! Handler tests for `tm secrets` (issue #7521).
//!
//! Why: sibling file (declared via `#[path = "secrets_tests.rs"]`) so the
//! handler module stays inside the 500-SLOC production cap.
//! What: every test runs against a `KeychainBackend` bound to an injected
//! `MemoryKeyStore` and a temp-dir index — no test here touches a real
//! keychain, prompts, or the operator's config file.
//! Test: itself.

use std::sync::Arc;

use tempfile::TempDir;
use trusty_common::credentials::{KeyStore, MemoryKeyStore, index_path_at};

use super::*;

const GROUP: &str = "bobmatnyc/trusty-tools";
/// Stand-in secret; asserted absent from every printed line.
const FAKE_VALUE: &str = "fake-value-3a91cc";

/// A backend over an inspectable memory store and a throwaway index.
fn fixture() -> (TempDir, Arc<MemoryKeyStore>, KeychainBackend) {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(MemoryKeyStore::new());
    let backend = KeychainBackend::with_store(
        GROUP,
        Arc::clone(&store) as Arc<dyn KeyStore>,
        index_path_at(tmp.path(), GROUP),
    )
    .unwrap();
    (tmp, store, backend)
}

/// Why: `--value -` is the only scripted entry path; it must read all of
/// stdin, strip the trailing newline a `printf`/pipe adds, and refuse an
/// empty pipe rather than storing an empty secret.
/// Test: itself.
#[test]
fn secrets_add_reads_value_from_stdin_when_dash() {
    assert_eq!(
        value_source(Some("-"), false).unwrap(),
        ValueSource::Stdin,
        "`--value -` must read stdin even when stdin is not a terminal"
    );
    let value = read_value_from_reader(format!("  {FAKE_VALUE}\n").as_bytes()).unwrap();
    assert_eq!(value, FAKE_VALUE);
    assert!(
        read_value_from_reader("  \n".as_bytes()).is_err(),
        "a blank pipe must be refused"
    );
}

/// Why: the whole point of the command — a stored value must not reach
/// stdout, and must not reach an error message either.
/// Test: itself.
#[test]
fn secrets_add_never_prints_the_value() {
    let (_tmp, store, backend) = fixture();
    let lines = add_into(&backend, "API_KEY", FAKE_VALUE).unwrap();
    let rendered = lines.join("\n");
    assert!(
        !rendered.contains(FAKE_VALUE),
        "add printed the value: {rendered}"
    );
    assert!(rendered.contains("API_KEY"));
    assert!(rendered.contains("trusty/bobmatnyc/trusty-tools"));
    assert_eq!(store.get("API_KEY").as_deref(), Some(FAKE_VALUE));

    // The failure path must not quote the value either.
    let err = add_into(&backend, "bad key", FAKE_VALUE).unwrap_err().to_string();
    assert!(!err.contains(FAKE_VALUE), "error leaked the value: {err}");
}

/// Why: without a TTY and without `--value -` there is no safe place to read
/// from, so the command must stop with instructions instead of hanging on a
/// prompt nothing will answer.
/// Test: itself.
#[test]
fn secrets_add_refuses_when_no_tty_and_no_stdin_flag() {
    let err = value_source(None, false).unwrap_err().to_string();
    assert!(
        err.contains("--value -"),
        "the refusal must name how to supply the value: {err}"
    );
    assert_eq!(value_source(None, true).unwrap(), ValueSource::Prompt);
}

/// Why: a literal `--value <secret>` would land the value in `ps` output and
/// shell history; it is refused rather than accepted with a warning.
/// Test: itself.
#[test]
fn secrets_add_refuses_a_literal_value_argument() {
    let err = value_source(Some(FAKE_VALUE), true).unwrap_err().to_string();
    assert!(!err.contains(FAKE_VALUE), "refusal leaked the value: {err}");
    assert!(err.contains("never accepted as a command argument"));
}

/// Why: `list` is the verb most likely to be pasted into a transcript, so it
/// must render names and counts only.
/// Test: itself.
#[test]
fn secrets_list_prints_names_only() {
    let (_tmp, _store, backend) = fixture();
    add_into(&backend, "B_KEY", FAKE_VALUE).unwrap();
    add_into(&backend, "A_KEY", FAKE_VALUE).unwrap();

    let rendered = list_lines(&backend).unwrap().join("\n");
    assert!(!rendered.contains(FAKE_VALUE), "list leaked a value: {rendered}");
    assert!(rendered.contains("A_KEY") && rendered.contains("B_KEY"));
    assert!(rendered.contains("2 keys"));
}

/// Why: a removal must report the name it dropped without reading the value
/// it dropped.
/// Test: itself.
#[test]
fn secrets_remove_reports_the_name_it_dropped() {
    let (_tmp, store, backend) = fixture();
    add_into(&backend, "DROP", FAKE_VALUE).unwrap();
    let line = remove_from(&backend, "DROP").unwrap();
    assert!(line.contains("DROP") && !line.contains(FAKE_VALUE));
    assert!(store.get("DROP").is_none());
    assert!(backend.list().unwrap().is_empty());
}

/// Why: slice 1 stores everything in the keychain; accepting `--provider
/// onepassword` would put values somewhere the operator did not choose.
/// Test: itself.
#[test]
fn secrets_configure_rejects_unsupported_provider() {
    assert!(validate_provider("keychain").is_ok());
    for provider in ["onepassword", "keeper", "vault", ""] {
        let err = validate_provider(provider).unwrap_err().to_string();
        assert!(
            err.contains("#7519"),
            "the refusal must name the issue that adds it: {err}"
        );
    }
}

/// Why: `doctor` runs before anything is configured, so it must render from
/// a probe result alone — no prompt, no unlock, no value.
/// Test: itself.
#[test]
fn secrets_doctor_reports_probe_result_without_prompting() {
    let healthy = DoctorReport {
        backend: KEYCHAIN_BACKEND.to_string(),
        group: GROUP.to_string(),
        keychain_reachable: true,
        indexed_names: 2,
        index_path: "/home/bob/.trusty-tools/trusty-mpm/secrets-index/x.json".to_string(),
    };
    let rendered = doctor_lines(&healthy).join("\n");
    assert!(healthy.healthy());
    assert!(rendered.contains("backend: keychain"));
    assert!(rendered.contains(&format!("group: {GROUP}")));
    assert!(rendered.contains("keychain: reachable"));
    assert!(rendered.contains("indexed names: 2"));

    let unreachable = DoctorReport {
        keychain_reachable: false,
        ..healthy
    };
    assert!(!unreachable.healthy(), "an unreachable probe is not healthy");
    assert!(doctor_lines(&unreachable).join("\n").contains("UNREACHABLE"));
}
