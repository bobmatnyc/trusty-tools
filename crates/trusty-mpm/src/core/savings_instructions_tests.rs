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

/// The roster-source stand-in for a fixture with no deployed agents (#7616).
///
/// Why: the real resolver reads this machine's global agent tiers, so calling it
/// from the suite would make every assertion depend on what the operator has
/// installed. Injecting the byte count keeps the suite hermetic, which is the
/// same reason `price` is injected.
fn no_roster(_project_dir: &std::path::Path) -> usize {
    0
}

/// A compiled prompt that is a genuine fold: far smaller than the source set,
/// but large enough to be a real assembly of the bundled sections.
///
/// Why (#7491): the producer now refuses anything below
/// [`min_plausible_compiled_bytes`], so a four-byte stand-in no longer stands in
/// for a compiled prompt. This is the smallest fixture the contract accepts.
fn plausible_prompt() -> String {
    "x".repeat(min_plausible_compiled_bytes().max(1))
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
    // #7179: the percent denominator is the folded source set's own token
    // count — 60,000 bytes at four bytes per token is 15,000 tokens.
    assert_eq!(row.tokens_before, 15_000);
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

/// Why (#6972): this producer runs at session launch, off the very chain that
/// produced the session's `--model` flag, so its config lookup is authoritative
/// rather than the divert producer's last-resort guess. The row says which,
/// so an operator reading the ledger does not have to know that.
/// Test: itself.
#[test]
fn instruction_compression_row_names_its_model_source() {
    let row = instruction_compression_row("sess-a", 60_000, 20_000, sonnet_price).expect("a row");
    assert_eq!(
        row.model_source,
        crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG
    );
    assert_eq!(row.model_source, "launch-config");
    assert_ne!(
        row.model_source,
        crate::core::session_model::MODEL_SOURCE_CONFIG_FALLBACK,
        "a launch-time config read must not be labelled a fallback"
    );
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

/// Why (#7514): the resolver counted directories instead of checking their
/// names, so any path four segments deep resolved to a session. The owner's
/// ledger carried 228 rows reading `"session_id":"b" … compiled 13 B` — `b` is
/// the second directory of `<tmp>/a/b/INSTRUCTIONS-COMPILED.md`, this crate's
/// own `write_compiled_prompt_to_creates_parent_dirs` fixture, and 13 B is that
/// fixture's `COMPILED-BODY` body.
/// FAILS BEFORE THIS CHANGE: every path below resolved to a session id.
/// Test: itself.
#[test]
fn session_and_root_rejects_a_path_that_is_not_under_sessions() {
    for not_a_compiled_prompt in [
        "/tmp/x/a/b/INSTRUCTIONS-COMPILED.md",
        "/repos/t/.trusty-mpm/framework/sess-1/INSTRUCTIONS-COMPILED.md",
        "/repos/t/other/sessions/sess-1/INSTRUCTIONS-COMPILED.md",
        "/repos/t/.trusty-mpm/sessions/sess-1/last-instructions.md",
    ] {
        assert!(
            session_and_root(std::path::Path::new(not_a_compiled_prompt)).is_none(),
            "{not_a_compiled_prompt} is not a compiled-prompt path"
        );
    }
}

/// Why (#7514): the producer's whole input is a destination path, so a
/// destination that is not a compiled prompt must produce nothing at all —
/// neither a ledger row nor a staged one. The body here is deliberately ABOVE
/// the #7491 floor, so what the assertion pins is the PATH check and not the
/// floor; the floor's own guards are unchanged and still cover a short file.
/// FAILS BEFORE THIS CHANGE: the ledger gained a row keyed `b`.
/// Test: itself.
#[test]
fn a_malformed_compiled_prompt_path_records_no_row() {
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let tmp = tempfile::tempdir().expect("temp dir");
    let dest = tmp
        .path()
        .join("a")
        .join("b")
        .join("INSTRUCTIONS-COMPILED.md");
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("fixture dirs");
    let plausible = plausible_prompt_above_the_floor();
    std::fs::write(&dest, &plausible).expect("fixture");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &plausible,
        Some("c-7514".to_string()),
        no_roster,
        sonnet_price,
    );

    assert!(
        !ledger.exists(),
        "a destination that is not a compiled prompt must append no row: {}",
        std::fs::read_to_string(&ledger).unwrap_or_default()
    );
    assert!(
        !crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest).exists(),
        "nor stage one"
    );
    assert!(
        !framework_root
            .path()
            .join("usage")
            .join("no-fold-warned")
            .exists(),
        "nor leave a no-fold marker behind"
    );
}

/// A compiled-prompt body that clears the #7491 floor and still folds.
///
/// Why: a fixture below the floor would make every assertion about the floor
/// rather than about the property under test.
fn plausible_prompt_above_the_floor() -> String {
    "x".repeat(min_plausible_compiled_bytes() + 137)
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
        folded_source_bytes(dir.path(), 0),
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
    let baseline = folded_source_bytes(dir.path(), 0);
    let body = "Ship it. Skip the ceremony.";
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        format!(
            "<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n{body}\n<!-- TRUSTY-MPM: WORKFLOW END -->\n"
        ),
    )
    .expect("write CLAUDE.md");

    let with_override = folded_source_bytes(dir.path(), 0);
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

/// Build a compiled-prompt destination under a fresh harness root.
///
/// Why: every keying test needs the same
/// `<root>/.trusty-mpm/sessions/<scope>/INSTRUCTIONS-COMPILED.md` shape, and the
/// producer reads the root as a project directory, so it must exist on disk.
/// What: creates the session directory and returns the destination path.
/// Test: used by `the_row_is_keyed_by_the_claude_session_id`.
fn compiled_prompt_dest(root: &std::path::Path, scope: &str) -> std::path::PathBuf {
    let dir = root.join(".trusty-mpm").join("sessions").join(scope);
    std::fs::create_dir_all(&dir).expect("session dir");
    dir.join("INSTRUCTIONS-COMPILED.md")
}

/// Why (#7209): the `💸` segment folds this ledger by the session id Claude Code
/// sends the statusline on stdin, while the compiled prompt lives under the
/// managed scope (`local` for an unmanaged launch). A row keyed by that
/// directory name is a row the segment can never match, so the segment renders
/// nothing at all — the owner-reported symptom.
/// Test: itself.
#[test]
fn the_row_is_keyed_by_the_claude_session_id() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &plausible_prompt(),
        Some("claude-abc-123".to_string()),
        no_roster,
        sonnet_price,
    );

    let written = std::fs::read_to_string(&ledger).expect("the ledger must exist");
    assert!(
        written.contains("\"session_id\":\"claude-abc-123\""),
        "the row must be keyed by the Claude Code session id: {written}"
    );

    let matched = crate::core::savings::fold_session(&ledger, "claude-abc-123");
    assert!(
        !matched.is_zero(),
        "the statusline fold under the Claude Code id must find the row: {matched:?}"
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "local").is_zero(),
        "nothing may be attributed to the compiled-prompt directory name"
    );
}

/// Why (#7245): this producer ALWAYS lacks the Claude Code id — it runs before
/// `claude` is spawned — so #7209's fallback to the directory name was not an
/// edge case but every row, and the statusline folds by no such key. Writing one
/// anyway put a row on the ledger that no surface could ever attribute. It is
/// staged for the hook instead, and the ledger stays untouched until that hook
/// can key it.
/// Test: itself.
#[test]
fn no_claude_id_stages_the_row_instead_of_writing_an_unfoldable_one() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "sess-42");

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &plausible_prompt(),
        None,
        no_roster,
        sonnet_price,
    );

    let ledger = crate::core::savings::savings_log_in(framework_root.path());
    assert!(
        !ledger.exists(),
        "a row nothing can fold must not reach the ledger"
    );
    let staged = crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest);
    let text = std::fs::read_to_string(&staged).expect("the row must be staged for the hook");
    assert!(
        text.contains("\"session_id\":\"sess-42\""),
        "the staged row keeps the compile-time id until the hook re-keys it: {text}"
    );
}

/// Why (#7245, required acceptance): the closure condition — a session that never
/// exports `CLAUDE_CODE_SESSION_ID` to the compiling `tm` process still produces
/// a row the statusline can fold. This drives the real producer and the real
/// hook-side claim end to end, including the second invocation that must not
/// duplicate the row.
/// Test: itself.
#[test]
fn a_staged_row_becomes_foldable_at_the_first_hook_invocation() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "m1");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &plausible_prompt(),
        None,
        no_roster,
        sonnet_price,
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "c1").is_zero(),
        "before the hook there is nothing for the statusline to fold"
    );

    for _ in 0..2 {
        crate::core::savings_sidecar::emit_staged_row(&ledger, framework_root.path(), &dest, "c1");
    }

    let folded = crate::core::savings::fold_session(&ledger, "c1");
    assert!(
        !folded.is_zero(),
        "the statusline fold for the launched Claude session must find the row: {folded:?}"
    );
    assert_eq!(
        folded.rows, 1,
        "two hook invocations must leave exactly one row for the pair: {folded:?}"
    );
    assert!(
        crate::core::savings::fold_session(&ledger, "m1").is_zero(),
        "nothing may stay attributed to the managed session id"
    );
}

/// Why (#7245): for a project that overrides no instruction section the composer
/// only ADDS context, so this decline is permanent and no row can ever be
/// written. At `debug!` that left the segment's absence unexplained. The
/// producer must state it — once — and still write nothing.
/// Test: itself.
#[test]
fn a_prompt_that_folds_nothing_warns_once_and_writes_no_row() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "m1");
    let bulky = "x".repeat(folded_source_bytes(project.path(), 0) + 1);

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &bulky,
        None,
        no_roster,
        sonnet_price,
    );

    assert!(
        !crate::core::savings::savings_log_in(framework_root.path()).exists(),
        "a prompt that folded nothing must write no row"
    );
    assert!(
        !crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest).exists(),
        "a prompt that folded nothing must stage nothing either"
    );
    assert!(
        !crate::core::savings_sidecar::warn_no_fold_once(
            framework_root.path(),
            project.path(),
            folded_source_bytes(project.path(), 0),
            bulky.len(),
        ),
        "the producer must already have warned for this project and byte pair"
    );
}

/// Why (#7411): staging is the producer's only route to the ledger for a normal
/// launch, so a staged row that was never written leaves the session with a
/// blank `💸` segment for its whole life. The compiled prompt on disk is the
/// same input the producer measured, so the hook can redo the measurement
/// rather than depend on a file that may not be there.
/// Test: itself.
#[test]
fn rederiving_from_the_compiled_prompt_appends_under_the_session_id() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    std::fs::write(&dest, plausible_prompt()).expect("compiled prompt");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    assert!(
        rederive_from_compiled_prompt(framework_root.path(), &dest, "c1"),
        "the compiled prompt on disk must be enough to produce the row"
    );
    let folded = crate::core::savings::fold_session(&ledger, "c1");
    assert!(
        !folded.is_zero(),
        "the re-derived row must fold under the Claude session id: {folded:?}"
    );
    assert!(
        !crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest).exists(),
        "a re-derivation appends; it must never stage a second copy"
    );
}

/// Why (#7411): the re-derivation runs the ordinary producer, so it inherits the
/// decline for a project that folds nothing. Reporting a row there would put a
/// fabricated figure on the status bar.
/// Test: itself.
#[test]
fn rederiving_a_prompt_that_folds_nothing_appends_nothing() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    // #7616: the re-derivation resolves this machine's roster onto the source
    // side, so a fixture sized against the bundled sections alone would now
    // FOLD and take the other branch.
    let bulky = "x".repeat(ambient_source_bytes(project.path()) + 1);
    std::fs::write(&dest, &bulky).expect("compiled prompt");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    assert!(
        !rederive_from_compiled_prompt(framework_root.path(), &dest, "c1"),
        "a prompt that folded nothing must report no row"
    );
    assert!(crate::core::savings::fold_session(&ledger, "c1").is_zero());
}

/// Why (#7491): the row's `compiled_bytes` must be the size of the compiled
/// prompt the producer actually measured. The owner's ledger carried
/// `sources 22559 B - compiled 13 B` — a claimed ~100% reduction — so nothing
/// pinned that the number describes the file at the compiled-prompt path.
/// Test: itself.
#[test]
fn the_row_reports_the_compiled_prompts_own_size() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    // A known size, comfortably above the floor and below the source set.
    let staged_bytes = min_plausible_compiled_bytes() + 137;
    std::fs::write(&dest, "x".repeat(staged_bytes)).expect("compiled prompt");
    let on_disk = std::fs::metadata(&dest).expect("compiled prompt").len() as usize;
    assert_eq!(
        on_disk, staged_bytes,
        "the fixture must be the size it claims"
    );
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    assert!(
        rederive_from_compiled_prompt(framework_root.path(), &dest, "c1"),
        "a plausible compiled prompt must produce a row"
    );

    let written = std::fs::read_to_string(&ledger).expect("the ledger must exist");
    assert!(
        written.contains(&format!("compiled {on_disk} B")),
        "the row must report the compiled prompt's own size ({on_disk} B): {written}"
    );
    assert!(
        !written.contains("compiled 13 B"),
        "the #7491 placeholder measurement must be unreachable: {written}"
    );
}

/// Why (#7491): a stub, a truncated write, or a stale file at the
/// compiled-prompt path is smaller than any real compiled prompt, and the only
/// plausibility test was `compiled < sources` — which such a file passes
/// trivially, producing a near-100% saving that inflates the `💸` percentage for
/// as long as the append-only ledger keeps it. A missing file must likewise
/// produce nothing.
/// Test: itself.
#[test]
fn a_stub_compiled_prompt_writes_no_row() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let dest = compiled_prompt_dest(project.path(), "local");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    // The exact shape the owner's ledger recorded: 13 bytes against a 22 kB
    // source set.
    std::fs::write(&dest, "placeholder\n\n").expect("compiled prompt");
    assert_eq!(
        std::fs::metadata(&dest).expect("compiled prompt").len(),
        13,
        "the fixture reproduces the reported 13-byte compiled row"
    );
    assert!(
        !rederive_from_compiled_prompt(framework_root.path(), &dest, "c1"),
        "a stub compiled prompt must report no row"
    );
    assert!(
        !ledger.exists(),
        "a stub compiled prompt must not reach the ledger: {}",
        std::fs::read_to_string(&ledger).unwrap_or_default()
    );
    assert!(
        !crate::core::savings_sidecar::pending_row_path_in(framework_root.path(), &dest).exists(),
        "a stub compiled prompt must not be staged either"
    );

    // A path with no file at all is the other arm: still no row, no panic.
    let missing = compiled_prompt_dest(project.path(), "gone");
    std::fs::remove_file(&missing).ok();
    assert!(
        !rederive_from_compiled_prompt(framework_root.path(), &missing, "c2"),
        "a missing compiled prompt must report no row"
    );
    assert!(!ledger.exists(), "a missing file must write nothing");
}

/// Why (#7209): the producers must read one variable name. A rename on one side
/// only would silently unmatch every row the other writes.
/// Test: itself.
#[test]
fn claude_code_session_id_names_the_harness_variable() {
    assert_eq!(
        crate::core::savings::CLAUDE_CODE_SESSION_ID_ENV,
        "CLAUDE_CODE_SESSION_ID"
    );
}

// ---------------------------------------------------------------------------
// #7616 — the no-override fold, end to end.
//
// Why these live here rather than in `instruction_fold_tests.rs`: the defect was
// never in one function. It was that the composer performed no transformation
// AND the producer counted the roster only on the delivered side, so
// `compiled >= sources` was structurally permanent for a project that overrides
// nothing. Proving the fix means composing the real prompt and running the real
// producer over it.
// ---------------------------------------------------------------------------

/// Write `count` agent files into `dir`, each with the frontmatter the roster
/// scanner requires.
///
/// Why: the roster dedup is the larger half of the no-override fold, and it only
/// exists when agents are deployed. A fixture tier is how the suite exercises it
/// without reading this machine's global agent directories.
fn deploy_agents(dir: &std::path::Path, count: usize) {
    std::fs::create_dir_all(dir).expect("agent tier");
    for index in 0..count {
        std::fs::write(
            dir.join(format!("fixture-agent-{index}.md")),
            format!(
                "---\nname: fixture-agent-{index}\nrole: engineer\nmodel: sonnet\n---\n\nBody.\n"
            ),
        )
        .expect("agent file");
    }
}

/// The no-override prompt and the roster bytes the composer read to produce it.
///
/// Why: every assertion below needs the same numbers, taken from the same
/// composition, or they would be measuring different prompts.
/// What: composes through `resolve_pm_prompt_with_roster` — the real entry point
/// for a project with no `CLAUDE.md` — with a roster deduped against a harness
/// tier holding every agent, which is the shape a real machine has (#4513).
fn no_override_composition(project: &std::path::Path) -> (String, usize) {
    let tier = project.join(".claude").join("agents");
    deploy_agents(&tier, 40);
    let dirs = vec![tier.clone()];

    let delivered = crate::core::delegation_authority::roster_section_from_tiers(&dirs, &dirs)
        .expect("a deployed tier renders a roster");
    let read = crate::core::delegation_authority::roster_section_from_dirs(&dirs)
        .expect("the undeduped roster renders too");

    let (prompt, _) =
        crate::core::instruction_overrides::resolve_pm_prompt_with_roster(project, || {
            Some(delivered.clone())
        });
    (prompt, read.len())
}

/// #7616 REGRESSION: for a project that overrides no section, the compiled
/// prompt must come out SMALLER than the instruction bodies the composer read.
///
/// FAILS BEFORE THIS CHANGE. On the pre-fix commit the composer applied no fold
/// and the roster was counted only on the delivered side, so `compiled` was the
/// sources PLUS the roster and the stack profile — the field measurement was
/// 26,810 B of sources against a 26,695 B prompt, and the marker under
/// `no-fold-warned/` recorded compiled 26,736 B against sources 22,559 B.
/// Test: itself.
#[test]
fn a_no_override_project_folds_below_its_authored_sources() {
    let project = tempfile::tempdir().expect("temp project");
    let (prompt, roster_read) = no_override_composition(project.path());

    let sources = folded_source_bytes(project.path(), roster_read);
    let compiled = prompt.len();

    assert!(
        compiled < sources,
        "the no-override fold must remove bytes: sources {sources} B, compiled \
         {compiled} B"
    );
}

/// #7616 REGRESSION: the reduction must be at least what the prose fold alone
/// recovers from the bundled corpus.
///
/// Why a corpus-derived floor rather than a round percentage: the roster half of
/// the fold scales with how many agents a machine deploys, so a fixed percentage
/// would pin the fixture rather than the mechanism. The prose half does not — it
/// is a property of the shipped sections — so it is the honest floor to assert.
/// FAILS BEFORE THIS CHANGE: the reduction was negative.
/// Test: itself.
#[test]
fn the_no_override_fold_clears_the_prose_floor() {
    let project = tempfile::tempdir().expect("temp project");
    let (prompt, roster_read) = no_override_composition(project.path());

    let sources = folded_source_bytes(project.path(), roster_read);
    let saved = sources.saturating_sub(prompt.len());

    let prose_floor: usize = crate::core::instruction_pipeline::SECTION_SOURCES
        .iter()
        .map(|(_, body)| {
            body.len() - crate::core::instruction_fold::fold_delivered_prompt(body).len()
        })
        .sum();

    assert!(prose_floor > 0, "the prose fold must recover something");
    assert!(
        saved >= prose_floor,
        "the fold saved {saved} B, below the {prose_floor} B the prose fold alone \
         recovers from the bundled sections"
    );
}

/// #7616 REGRESSION: a real ledger row lands for the no-override case, and its
/// `basis` carries the MEASURED byte counts.
///
/// Why the basis matters (#7514, #7584): the ledger has twice carried rows whose
/// numbers came from a fixture rather than a real composition. A row is only
/// evidence if the counts in it are the counts that were measured.
/// FAILS BEFORE THIS CHANGE: no row was written at all — the producer took the
/// `warn_no_fold_once` decline branch every time.
/// Test: itself.
#[test]
fn the_no_override_row_basis_carries_the_measured_bytes() {
    let project = tempfile::tempdir().expect("temp project");
    let framework_root = tempfile::tempdir().expect("temp framework root");
    let (prompt, roster_read) = no_override_composition(project.path());
    let dest = compiled_prompt_dest(project.path(), "local");
    let ledger = crate::core::savings::savings_log_in(framework_root.path());

    record_instruction_compression_to(
        framework_root.path(),
        &dest,
        &prompt,
        Some("claude-7616".to_string()),
        move |_| roster_read,
        sonnet_price,
    );

    let written = std::fs::read_to_string(&ledger).expect("a row must be written");
    let sources = folded_source_bytes(project.path(), roster_read);
    assert!(
        written.contains(&format!(
            "sources {sources} B - compiled {} B",
            prompt.len()
        )),
        "the basis must carry the measured counts, not a placeholder: {written}"
    );
    assert!(
        written.contains("claude-7616"),
        "the row must be keyed by the Claude session id: {written}"
    );
    assert!(
        !written.contains("\"session_id\":\"local\""),
        "a row keyed by the compiled-prompt directory is the #7209 defect: {written}"
    );
}

/// Why (#7616): "0 when no agent is deployed anywhere" is the branch that keeps
/// the savings figure able to come out honestly zero — a machine with no roster
/// folds no roster, and claiming otherwise would inflate every row. The ambient
/// entry point cannot assert it (it reads this machine's global tiers), so the
/// claim is asserted against the tier seam.
/// Test: itself.
#[test]
fn roster_source_bytes_are_zero_without_a_roster() {
    assert_eq!(
        roster_source_bytes_from(&[]),
        0,
        "no tier at all folds no roster"
    );

    let empty_tier = tempfile::tempdir().expect("temp tier");
    assert_eq!(
        roster_source_bytes_from(&[empty_tier.path().to_path_buf()]),
        0,
        "a tier that exists but deploys no agent folds no roster"
    );

    let absent = empty_tier.path().join("never-created");
    assert_eq!(
        roster_source_bytes_from(&[absent]),
        0,
        "an absent tier is normal, not a fold"
    );

    // The positive control: the same seam DOES count a tier that holds agents,
    // so the zeros above are the branch and not a broken call.
    let deployed = tempfile::tempdir().expect("temp tier");
    deploy_agents(deployed.path(), 3);
    assert!(
        roster_source_bytes_from(&[deployed.path().to_path_buf()]) > 0,
        "a tier holding agents must contribute source bytes"
    );
}

/// Why (#7616): the roster dedup is read-and-discarded source in exactly the
/// sense an overridden section body is. Counting it only on the delivered side
/// is what made the no-override case permanently zero.
/// Test: itself.
#[test]
fn the_roster_dedup_counts_as_folded_source() {
    let dir = tempfile::tempdir().expect("temp dir");
    let baseline = folded_source_bytes(dir.path(), 0);
    assert_eq!(
        folded_source_bytes(dir.path(), 1_234),
        baseline + 1_234,
        "the roster the composer read must land on the source side"
    );
}

/// Why (#7616): the `tm doctor` check reads this, and a project that has never
/// launched a session has no prompt to measure. Reporting a number there would
/// be inventing one.
/// Test: itself.
#[test]
fn the_fold_measurement_is_none_before_any_session_compiles() {
    let project = tempfile::tempdir().expect("temp project");
    assert!(measure_project_fold(project.path()).is_none());
}

/// Why (#7616): the check must measure THIS launch's prompt, so it reads the
/// newest compiled prompt rather than whichever directory enumerates first.
/// Test: itself.
#[test]
fn the_fold_measurement_reads_the_newest_compiled_prompt() {
    let project = tempfile::tempdir().expect("temp project");
    let dest = compiled_prompt_dest(project.path(), "local");
    std::fs::write(&dest, "x".repeat(4_096)).expect("compiled prompt");

    let (sources, compiled) = measure_project_fold(project.path()).expect("a measurement");
    assert_eq!(compiled, 4_096);
    assert!(
        sources >= min_plausible_compiled_bytes(),
        "the source side must carry the bundled corpus"
    );
}
