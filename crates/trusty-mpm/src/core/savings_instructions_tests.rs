//! Tests for the instruction/language-compression savings producer (#6958).
//!
//! Why: the producer's whole contract is "write a row only when the fold
//! genuinely removed something". A permissive implementation — one that writes a
//! row on every launch, or that substitutes a guessed price for an unknown
//! model — would put a fabricated figure on the operator's status bar.
//! What: the row builder is driven directly with hand-supplied byte counts and
//! an injected price, so every decline branch is asserted without a filesystem
//! or a resolved model.
//! Test: this file.

use super::*;

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<(String, f64)> {
    Some(("claude-sonnet-4-6".to_string(), 3.0))
}

/// Why (#6958, required acceptance): the common case is a project that
/// overrides nothing, where the composer ADDS generated context and the
/// delivered prompt is larger than its sources. Writing a row there would put a
/// number on the status bar that no fold produced.
/// Test: itself.
#[test]
fn no_row_when_the_compiled_prompt_is_not_smaller() {
    assert!(
        instruction_compression_row("sess-a", 26_617, 30_721, sonnet_price).is_none(),
        "a larger compiled prompt must write no row"
    );
    assert!(
        instruction_compression_row("sess-a", 26_617, 26_617, sonnet_price).is_none(),
        "an equal compiled prompt must write no row"
    );
    // A delta under one whole token is also nothing to report.
    assert!(
        instruction_compression_row("sess-a", 1_003, 1_000, sonnet_price).is_none(),
        "a sub-token delta must write no row"
    );
}

/// Why: the positive branch, with figures a reader can check by hand —
/// 40,000 bytes folded away is 10,000 tokens at four bytes each, which at
/// Sonnet's $3/Mtok input rate is $0.03.
/// Test: itself.
#[test]
fn a_folded_source_set_produces_a_row() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 10_000);
    assert!((row.cost_saved_usd - 0.03).abs() < 1e-9);
    assert!(
        row.basis.contains("60000") && row.basis.contains("20000"),
        "the basis must carry both byte counts: {}",
        row.basis
    );
}

/// Why: the fold divisor is shared across producers so their rows are
/// comparable; a producer that picked its own would report an incomparable
/// number under the same total.
/// Test: itself.
#[test]
fn instruction_compression_tokens_use_the_shared_divisor() {
    let row = instruction_compression_row("sess-a", 8_000, 4_000, sonnet_price).expect("a row");
    assert_eq!(row.tokens_saved, (4_000.0 / BYTES_PER_TOKEN) as i64);
    assert_eq!(row.tokens_saved, 1_000);
}

/// Why: the fold reader filters on `technique` for a per-technique breakdown,
/// so the name the producer writes is part of the contract.
/// Test: itself.
#[test]
fn instruction_compression_row_carries_the_named_technique() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_INSTRUCTION_COMPRESSION);
    assert_eq!(row.technique, "instruction-compression");
}

/// Why (#6958): a model the shared price table does not know must decline the
/// row, never substitute a guessed rate. A fabricated price is worse than no
/// figure — it is a figure the operator would act on.
/// Test: itself.
#[test]
fn no_row_when_the_model_cannot_be_priced() {
    assert!(
        instruction_compression_row("sess-a", 60_000, 20_000, || None).is_none(),
        "an unpriceable model must write no row"
    );
    assert!(
        instruction_compression_row("sess-a", 60_000, 20_000, || Some(("m".into(), 0.0))).is_none(),
        "a zero rate must write no row"
    );
}

/// Why: the producer's only inputs are a destination path and a prompt, so the
/// session id and the harness root have to come out of that path correctly or
/// every row is attributed to the wrong session.
/// Test: itself.
#[test]
fn session_and_root_reads_the_compiled_prompt_path() {
    let dest = std::path::Path::new(
        "/repos/trusty-tools/.trusty-mpm/sessions/sess-42/INSTRUCTIONS-COMPILED.md",
    );
    let (session_id, root) = session_and_root(dest).expect("resolved");
    assert_eq!(session_id, "sess-42");
    assert_eq!(root, std::path::Path::new("/repos/trusty-tools"));
}

/// Why: a path that is not of the compiled-prompt shape must decline rather
/// than attribute a row to a directory name that is not a session.
/// Test: itself.
#[test]
fn session_and_root_rejects_a_short_path() {
    assert!(session_and_root(std::path::Path::new("INSTRUCTIONS-COMPILED.md")).is_none());
    assert!(session_and_root(std::path::Path::new("/a/b/INSTRUCTIONS-COMPILED.md")).is_none());
}

/// Why: the bundled sections are the floor of the source set, and a count that
/// silently dropped them would make every fold look like a saving.
/// Test: itself.
#[test]
fn folded_source_bytes_counts_the_bundled_sections() {
    let dir = tempfile::tempdir().expect("temp dir");
    let bundled: usize = crate::core::instruction_pipeline::SECTION_SOURCES
        .iter()
        .map(|(_, body)| body.len())
        .sum();
    assert!(bundled > 0, "the bundled corpus must not be empty");
    assert_eq!(
        folded_source_bytes(dir.path()),
        bundled,
        "a project with no CLAUDE.md contributes only the bundled sections"
    );
}

/// Why: an override body is a source the composer READ and partly discarded, so
/// it belongs on the source side. Leaving it out would make every overriding
/// project look like it folded nothing.
/// Test: itself.
#[test]
fn folded_source_bytes_adds_an_override_body() {
    let dir = tempfile::tempdir().expect("temp dir");
    let baseline = folded_source_bytes(dir.path());
    let body = "Ship it. Skip the ceremony.";
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        format!(
            "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n{body}\n<!-- TRUSTY-MPM: WORKFLOW END -->\n"
        ),
    )
    .expect("write CLAUDE.md");

    let with_override = folded_source_bytes(dir.path());
    assert_eq!(
        with_override,
        baseline + body.len(),
        "the override body must be counted on the source side"
    );
}

/// Why: the price the producer uses must come from the shared table, not a
/// fourth copy. Asserting the resolved rate against a direct table lookup is
/// what pins that, without depending on which model this machine is configured
/// for.
/// Test: itself.
#[test]
fn resolve_pm_price_agrees_with_the_shared_table() {
    // A machine configured for a model outside the table resolves to `None`,
    // which declines the row — the documented behaviour, not a test failure.
    if let Some((model, rate)) = resolve_pm_price() {
        let table = trusty_common::inference::pricing(&model)
            .expect("the resolved model must be one the table knows");
        assert_eq!(rate, table.input, "the rate must be the table's input rate");
        assert!(rate > 0.0, "a priced model must have a positive input rate");
    }
}
