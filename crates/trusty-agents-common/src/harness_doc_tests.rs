//! Tests for [`super::HarnessDoc`]: the canonical markers of the repository's
//! harness docs, the legacy bundle key, and the missing-file error arm.

use std::sync::OnceLock;

use trusty_common::content::{ContentLock, LOCK_FILE_NAME};
use trusty_common::integrity::Sha256Digest;

use super::*;
use crate::agent_content::tests::{fake_checkout, repo_content};
use crate::agent_content::{DevOverride, resolve_content_in};

/// The repository's harness docs, loaded once per test binary.
fn doc() -> &'static HarnessDoc {
    static DOC: OnceLock<HarnessDoc> = OnceLock::new();
    DOC.get_or_init(|| HarnessDoc::load(&repo_content()).expect("repo harness docs"))
}

#[test]
fn agnostic_non_empty() {
    assert!(!doc().agnostic().trim().is_empty());
}

#[test]
fn agnostic_contains_claude_code_glyph() {
    // The ✻ glyph is the canonical Claude Code working signal (DOC-21 §5.1)
    assert!(
        doc().agnostic().contains('✻'),
        "agnostic section must document the ✻ glyph"
    );
}

/// #5129: these sections are the session manager's instructions for which
/// stderr marker to watch, so they are pinned to the constant tcode and
/// tagent actually emit — never to a copy of its text, which is how the
/// doc and the producer drifted apart unnoticed.
#[test]
fn harness_doc_names_the_relay_prefix() {
    let marker = crate::events::EVENT_LINE_PREFIX.trim_end();
    let full = doc().harness_understanding();
    for (section, text) in [
        ("agnostic", doc().agnostic()),
        ("tcode", doc().tcode()),
        ("overseer", doc().overseer()),
        ("full doc", full.as_str()),
    ] {
        assert!(
            text.contains(marker),
            "{section} must document the `{marker}` NDJSON relay prefix \
             (DOC-21 §5.2) — it is what the emitting harness writes"
        );
    }
}

#[test]
fn mpm_sm_non_empty() {
    assert!(!doc().mpm_session_manager().trim().is_empty());
}

#[test]
fn mpm_sm_contains_raw_observation() {
    let sm = doc().mpm_session_manager();
    assert!(
        sm.contains("RawObservation") || sm.contains("raw observation") || sm.contains("raw pane"),
        "SM section must reference the RawObservation/raw-pane two-tier model"
    );
}

#[test]
fn tcode_non_empty() {
    assert!(!doc().tcode().trim().is_empty());
}

#[test]
fn overseer_non_empty() {
    assert!(!doc().overseer().trim().is_empty());
}

#[test]
fn overseer_contains_flag_for_human() {
    let overseer = doc().overseer();
    assert!(
        overseer.contains("FlagForHuman") || overseer.contains("flag_for_human"),
        "overseer section must reference FlagForHuman escalation"
    );
}

#[test]
fn full_doc_contains_all_markers() {
    let full = doc().harness_understanding();
    assert!(full.contains('✻'), "full doc must contain ✻ glyph");
    assert!(
        full.contains("FlagForHuman") || full.contains("flag_for_human"),
        "full doc must contain FlagForHuman"
    );
}

/// Every section's distinctive body phrase reaches the joined doc.
#[test]
fn full_doc_sum_of_parts() {
    let full = doc().harness_understanding();
    for (section, anchor) in [
        ("AGNOSTIC", "WHEN-TO-INTERVENE"),
        ("MPM_SM", "RawObservation"),
        ("TCODE", "structured NDJSON event lines"),
        ("OVERSEER", "HarnessSource::Code"),
    ] {
        assert!(
            full.contains(anchor),
            "full doc must contain {section} body phrase '{anchor}'"
        );
    }
}

/// #9011: a source missing one harness doc is `Missing`, naming the file —
/// never a doc with an empty section.
#[test]
fn a_missing_harness_doc_is_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    fake_checkout(root.path());
    let dir = root
        .path()
        .join("content/instructions/harness_understanding");
    for file in [
        "HARNESS_AGNOSTIC.md",
        "HARNESS_MPM_SM.md",
        "HARNESS_TCODE.md",
    ] {
        std::fs::write(dir.join(file), "# section\n").expect("doc");
    }
    let content = crate::agent_content::checkout_content(root.path()).expect("checkout");
    let err = HarnessDoc::load(&content).expect_err("HARNESS_OVERSEER.md is absent");
    match &err {
        AgentContentError::Missing { path, .. } => assert_eq!(
            path,
            "instructions/harness_understanding/HARNESS_OVERSEER.md"
        ),
        other => panic!("expected Missing, got {other:?}"),
    }
    // #9396: a checkout is fixed in the checkout.
    assert!(err.to_string().contains("git pull"), "{err}");
}

/// A content-v0.1.0 bundle carries the docs under the old top-level
/// `harness_understanding/` key; [`HARNESS_DOC_DIRS`] still finds them.
#[test]
fn the_legacy_bundle_key_still_resolves() {
    const TAG: &str = "content-v0.1.0";
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let manifest = format!("tag = \"{TAG}\"\nschema_major = 1\n");
    let mut entries = vec![("bundle-manifest.toml".to_string(), manifest)];
    for file in SECTION_FILES {
        entries.push((
            format!("harness_understanding/{file}"),
            format!("# {file}\n"),
        ));
    }
    for (path, data) in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data.as_bytes())
            .expect("append");
    }
    let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
    let cache = tempfile::tempdir().expect("tempdir");
    let lock = ContentLock::new(TAG, Sha256Digest::of_bytes(&bytes)).expect("lock");
    std::fs::write(cache.path().join(lock.bundle_file_name()), &bytes).expect("bundle");
    lock.store(&cache.path().join(LOCK_FILE_NAME))
        .expect("store");

    let content = resolve_content_in(cache.path(), DevOverride::Off).expect("installed bundle");
    let doc = HarnessDoc::load(&content).expect("legacy key resolves");
    assert_eq!(doc.overseer(), "# HARNESS_OVERSEER.md\n");
    assert!(
        doc.harness_understanding()
            .contains("# HARNESS_AGNOSTIC.md")
    );
}
