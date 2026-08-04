//! Tests for the `CLAUDE.md` writer (issue #4754).
//!
//! Every test here asserts against the READER (`claude_md_sections::scan_project`)
//! rather than against a string the writer also produced. A writer test that
//! only checked the writer's own output would pass happily while emitting blocks
//! the reader declines — the advertised-but-unread failure (#381) this pairing
//! exists to prevent. Where a test does inspect raw bytes it is because the
//! property under test IS a byte property (preservation, line endings).

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::core::claude_md_sections::{REASON_DUPLICATE, scan_project};

/// A project root with no `CLAUDE.md` yet.
fn project() -> TempDir {
    TempDir::new().expect("temp project dir")
}

/// The host path inside a scratch project.
fn host(dir: &Path) -> PathBuf {
    dir.join("CLAUDE.md")
}

/// Read the host, or empty string when absent.
fn read(dir: &Path) -> String {
    std::fs::read_to_string(host(dir)).unwrap_or_default()
}

/// Seed the host with exact bytes.
fn seed(dir: &Path, text: &str) {
    std::fs::write(host(dir), text).expect("seed CLAUDE.md");
}

/// Count `START` marker lines for one section in the host.
fn start_marker_count(dir: &Path, section: SectionId) -> usize {
    let needle = format!("TRUSTY-MPM: {} START", section_token(section));
    read(dir).lines().filter(|l| l.contains(&needle)).count()
}

// ---------------------------------------------------------------------------
// Behaviour 1 — write a named section-override block
// ---------------------------------------------------------------------------

/// The written block must be one the READER accepts, with the body intact.
#[test]
fn writes_a_new_block_a_reader_accepts() {
    let dir = project();
    let outcome = write_section_override(dir.path(), SectionId::Workflow, "CUSTOM WORKFLOW")
        .expect("write accepted");
    assert_eq!(outcome, WriteOutcome::Created);

    let scanned = scan_project(dir.path());
    assert_eq!(
        scanned.overrides.len(),
        1,
        "reader must see exactly one override"
    );
    assert_eq!(scanned.overrides[0].section, SectionId::Workflow);
    assert_eq!(scanned.overrides[0].body, "CUSTOM WORKFLOW");
    assert!(
        scanned.diagnostics.is_empty(),
        "a freshly written block must raise no diagnostics: {:?}",
        scanned.diagnostics
    );
}

/// The emitted `v=` must be the version the reader implements. Hardcoding `1`
/// in the writer would pass a round-trip test today and silently emit
/// unsupported blocks after the next bump; asserting the constant is what makes
/// that drift impossible.
#[test]
fn written_block_declares_the_readers_supported_version() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Memory, "BODY").expect("write accepted");

    let text = read(dir.path());
    assert!(
        text.contains(&format!("START v={SUPPORTED_VERSION} -->")),
        "block must stamp the reader's supported version, got:\n{text}"
    );
    assert_eq!(
        scan_project(dir.path()).overrides.len(),
        1,
        "and the reader must accept that version"
    );
}

/// The writer must target the reader's declared host, not a path of its own.
#[test]
fn writes_target_the_readers_only_host() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Search, "BODY").expect("write accepted");

    assert!(host(dir.path()).is_file(), "CLAUDE.md must exist");
    assert_eq!(
        HOST_FILES[0], "CLAUDE.md",
        "writer follows the reader's host list"
    );
}

/// A body is preserved exactly as authored through a full write/read round trip,
/// including interior blank lines and markdown structure.
#[test]
fn round_trips_through_the_reader() {
    let dir = project();
    let body = "# Heading\n\n- bullet one\n- bullet two\n\nTrailing paragraph.";
    write_section_override(dir.path(), SectionId::Enforcement, body).expect("write accepted");

    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides[0].body, body);
}

/// The host is created when absent — the ordinary first-write case.
#[test]
fn creates_the_host_when_absent() {
    let dir = project();
    assert!(!host(dir.path()).exists(), "premise: no CLAUDE.md yet");

    let outcome =
        write_section_override(dir.path(), SectionId::Identity, "ID").expect("write accepted");
    assert_eq!(outcome, WriteOutcome::Created);
    assert!(host(dir.path()).is_file());
}

// ---------------------------------------------------------------------------
// Behaviour 2 — idempotent update, never a stacked second copy
// ---------------------------------------------------------------------------

/// The stacking regression in one assertion: apply twice, exactly one block.
#[test]
fn applying_twice_leaves_exactly_one_block() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Workflow, "ONCE").expect("first write");
    write_section_override(dir.path(), SectionId::Workflow, "ONCE").expect("second write");

    assert_eq!(
        start_marker_count(dir.path(), SectionId::Workflow),
        1,
        "a repeat write must never stack a second block:\n{}",
        read(dir.path())
    );
    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1);
    assert!(
        !scanned
            .diagnostics
            .iter()
            .any(|d| d.reason == REASON_DUPLICATE),
        "no duplicate diagnostic may be raised: {:?}",
        scanned.diagnostics
    );
}

/// An identical repeat write is a reported no-op, not a silent rewrite.
#[test]
fn applying_the_same_override_twice_reports_unchanged() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Memory, "SAME").expect("first write");
    let after_first = read(dir.path());

    let outcome =
        write_section_override(dir.path(), SectionId::Memory, "SAME").expect("second write");
    assert_eq!(outcome, WriteOutcome::Unchanged);
    assert_eq!(read(dir.path()), after_first, "bytes must not move");
}

/// A changed body replaces the old one in place — the reader must see only the
/// new value, never the stale first-wins copy.
#[test]
fn replacing_updates_the_body_in_place() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Workflow, "OLD VALUE").expect("first write");
    let outcome =
        write_section_override(dir.path(), SectionId::Workflow, "NEW VALUE").expect("second write");
    assert_eq!(outcome, WriteOutcome::Replaced);

    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1);
    assert_eq!(
        scanned.overrides[0].body, "NEW VALUE",
        "the reader must resolve to the value just written"
    );
    assert!(
        !read(dir.path()).contains("OLD VALUE"),
        "the superseded body must be gone from the file"
    );
}

/// A host that ALREADY carries two blocks for one section (the pre-existing
/// stacked state) is collapsed to one by a write. The fixture seeds genuine
/// duplicates, so the test cannot pass unless the writer actually removes the
/// shadowed copy.
#[test]
fn replacing_collapses_duplicate_blocks_to_one() {
    let dir = project();
    let token = section_token(SectionId::Workflow);
    seed(
        dir.path(),
        &format!(
            "intro\n\n\
             <!-- TRUSTY-MPM: {token} START v=1 -->\nFIRST\n<!-- TRUSTY-MPM: {token} END -->\n\n\
             middle prose\n\n\
             <!-- TRUSTY-MPM: {token} START v=1 -->\nSECOND\n<!-- TRUSTY-MPM: {token} END -->\n\n\
             outro\n"
        ),
    );
    // Premise: the reader really does see a duplicate before the write.
    let before = scan_project(dir.path());
    assert!(
        before
            .diagnostics
            .iter()
            .any(|d| d.reason == REASON_DUPLICATE),
        "fixture must actually contain a duplicate the reader reports"
    );

    write_section_override(dir.path(), SectionId::Workflow, "MERGED").expect("write accepted");

    assert_eq!(
        start_marker_count(dir.path(), SectionId::Workflow),
        1,
        "duplicates must be collapsed:\n{}",
        read(dir.path())
    );
    let after = scan_project(dir.path());
    assert_eq!(after.overrides[0].body, "MERGED");
    assert!(
        after.diagnostics.is_empty(),
        "no diagnostics should survive the collapse: {:?}",
        after.diagnostics
    );
    let text = read(dir.path());
    for kept in ["intro", "middle prose", "outro"] {
        assert!(text.contains(kept), "unrelated prose `{kept}` must survive");
    }
}

// ---------------------------------------------------------------------------
// Behaviour 3 — decline CORE, and only CORE
// ---------------------------------------------------------------------------

/// `CORE` is refused and the host is left exactly as it was.
#[test]
fn core_is_declined_and_logged() {
    let dir = project();
    seed(dir.path(), "project prose\n");
    let before = read(dir.path());

    let err = write_section_override(dir.path(), SectionId::Core, "TAKE OVER")
        .expect_err("CORE must be refused");
    assert!(
        matches!(
            err,
            WriteRejection::Protected {
                section: SectionId::Core,
                tier: CustomizationTier::Fixed
            }
        ),
        "unexpected rejection: {err:?}"
    );
    assert_eq!(
        read(dir.path()),
        before,
        "a refusal must not touch the file"
    );
}

/// CORE is the ONLY protected section. Without this, `core_is_declined_and_logged`
/// would still pass if the writer refused everything — the guard would be named
/// but never exercised.
#[test]
fn every_other_section_is_writable() {
    let dir = project();
    let mut written = 0usize;

    for section in SectionId::CANONICAL {
        let result = write_section_override(dir.path(), section, &format!("BODY {section:?}"));
        if section == SectionId::Core {
            assert!(result.is_err(), "CORE must be refused");
            continue;
        }
        result.unwrap_or_else(|e| panic!("{section:?} must be writable, got {e:?}"));
        written += 1;
    }

    assert!(
        written >= 8,
        "expected the full non-core taxonomy, wrote {written}"
    );
    let scanned = scan_project(dir.path());
    assert_eq!(
        scanned.overrides.len(),
        written,
        "the reader must accept every block written: {:?}",
        scanned.diagnostics
    );
    assert!(
        !scanned
            .overrides
            .iter()
            .any(|o| o.section == SectionId::Core),
        "no CORE override may exist"
    );
}

// ---------------------------------------------------------------------------
// Behaviour 4 — preserve everything outside the markers verbatim
// ---------------------------------------------------------------------------

/// Prose around the block — including trailing whitespace the author chose —
/// survives a replace byte for byte.
#[test]
fn preserves_surrounding_content_byte_for_byte() {
    let dir = project();
    let token = section_token(SectionId::Workflow);
    let prefix = "# Project   \n\nLine with trailing spaces   \n\n";
    let suffix = "\n\n## After\n\ttab-indented\t\n";
    seed(
        dir.path(),
        &format!(
            "{prefix}<!-- TRUSTY-MPM: {token} START v=1 -->\nOLD\n<!-- TRUSTY-MPM: {token} END -->\n{suffix}"
        ),
    );

    write_section_override(dir.path(), SectionId::Workflow, "NEW").expect("write accepted");

    let text = read(dir.path());
    assert!(
        text.starts_with(prefix),
        "prefix must be byte-identical, got:\n{text:?}"
    );
    assert!(
        text.ends_with(suffix),
        "suffix must be byte-identical, got:\n{text:?}"
    );
}

/// `\r\n` endings outside the block are not normalised away.
#[test]
fn preserves_crlf_line_endings() {
    let dir = project();
    let token = section_token(SectionId::Memory);
    seed(
        dir.path(),
        &format!(
            "alpha\r\nbeta\r\n<!-- TRUSTY-MPM: {token} START v=1 -->\r\nOLD\r\n<!-- TRUSTY-MPM: {token} END -->\r\nomega\r\n"
        ),
    );

    write_section_override(dir.path(), SectionId::Memory, "NEW").expect("write accepted");

    let text = read(dir.path());
    assert!(text.starts_with("alpha\r\nbeta\r\n"), "got:\n{text:?}");
    assert!(text.ends_with("omega\r\n"), "got:\n{text:?}");
    assert_eq!(scan_project(dir.path()).overrides[0].body, "NEW");
}

/// Appending to existing prose keeps that prose and separates the block.
#[test]
fn appends_after_existing_prose() {
    let dir = project();
    seed(dir.path(), "# Existing\n\nSome rules.\n");

    let outcome =
        write_section_override(dir.path(), SectionId::Search, "SEARCH RULES").expect("write");
    assert_eq!(outcome, WriteOutcome::Inserted);

    let text = read(dir.path());
    assert!(text.starts_with("# Existing\n\nSome rules.\n"));
    assert_eq!(scan_project(dir.path()).overrides[0].body, "SEARCH RULES");
}

/// A file with no final newline must not have its last line swallowed into the
/// marker line.
#[test]
fn appends_newline_when_existing_file_lacks_trailing_newline() {
    let dir = project();
    seed(dir.path(), "no trailing newline");

    write_section_override(dir.path(), SectionId::Search, "BODY").expect("write accepted");

    let text = read(dir.path());
    assert!(
        text.starts_with("no trailing newline\n"),
        "last line must stay its own line, got:\n{text:?}"
    );
    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1, "{:?}", scanned.diagnostics);
}

// ---------------------------------------------------------------------------
// Behaviour 5 — fail toward more framework instruction, never a corrupt file
// ---------------------------------------------------------------------------

/// An empty or whitespace-only body never blanks a section.
#[test]
fn empty_body_is_refused() {
    let dir = project();
    for body in ["", "   ", "\n\t\n"] {
        let err = write_section_override(dir.path(), SectionId::Workflow, body)
            .expect_err("empty body must be refused");
        assert!(matches!(err, WriteRejection::EmptyBody { .. }), "{err:?}");
    }
    assert!(
        !host(dir.path()).exists(),
        "a refused write must not even create the host"
    );
}

/// A body carrying its own marker line is refused. Were it written, the nested
/// `START` would make the reader DISCARD the outer block — deleting the very
/// section the author was setting.
#[test]
fn body_containing_a_marker_line_is_refused() {
    let dir = project();
    seed(dir.path(), "untouched\n");
    let before = read(dir.path());

    let hostile = "legit line\n<!-- TRUSTY-MPM: MEMORY START v=1 -->\nsmuggled\n";
    let err = write_section_override(dir.path(), SectionId::Workflow, hostile)
        .expect_err("a marker line in the body must be refused");
    assert!(
        matches!(err, WriteRejection::BodyContainsMarker { .. }),
        "{err:?}"
    );
    assert_eq!(read(dir.path()), before, "file must be untouched");
}

/// The marker screen uses the reader's recogniser, not a substring search:
/// prose that MENTIONS the marker inline is legal content and must still be
/// writable. Without this, the guard above could be satisfied by a blunt
/// `contains("TRUSTY-MPM:")` that silently blocks legitimate documentation.
#[test]
fn body_mentioning_the_marker_in_prose_is_written() {
    let dir = project();
    let body = "Use `<!-- TRUSTY-MPM: WORKFLOW START v=1 -->` to open a block.";

    write_section_override(dir.path(), SectionId::Workflow, body)
        .expect("prose mentioning a marker must be writable");

    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1, "{:?}", scanned.diagnostics);
    assert_eq!(scanned.overrides[0].body, body);
}

/// A host whose markers are unpaired is not edited at all. Splicing into a file
/// whose block extents are unknown is how a writer destroys content.
#[test]
fn an_unclosed_marker_blocks_the_write() {
    let dir = project();
    let token = section_token(SectionId::Workflow);
    seed(
        dir.path(),
        &format!("prose\n<!-- TRUSTY-MPM: {token} START v=1 -->\ndangling body\n"),
    );
    let before = read(dir.path());

    let err = write_section_override(dir.path(), SectionId::Workflow, "NEW")
        .expect_err("an unclosed marker must block the write");
    assert!(
        matches!(err, WriteRejection::HostMalformed { .. }),
        "{err:?}"
    );
    assert_eq!(read(dir.path()), before, "file must be byte-identical");
}

/// A host that cannot be read is reported, never overwritten. A directory at the
/// host path reproduces an unreadable host on every platform without depending
/// on file permissions.
#[test]
fn unreadable_host_is_reported_not_clobbered() {
    let dir = project();
    std::fs::create_dir(host(dir.path())).expect("directory at the host path");

    let err = write_section_override(dir.path(), SectionId::Workflow, "BODY")
        .expect_err("an unreadable host must be reported");
    assert!(matches!(err, WriteRejection::Io(_)), "{err:?}");
    assert!(
        host(dir.path()).is_dir(),
        "the writer must not have replaced it"
    );
}

/// Every refusal path leaves the host byte-identical — stated once, over all of
/// them, so a new rejection variant cannot quietly skip the guarantee.
#[test]
fn refusals_leave_the_file_byte_identical() {
    let token = section_token(SectionId::Workflow);
    let cases: Vec<(&str, String, SectionId, String)> = vec![
        (
            "protected section",
            "prose\n".to_string(),
            SectionId::Core,
            "BODY".to_string(),
        ),
        (
            "empty body",
            "prose\n".to_string(),
            SectionId::Workflow,
            "  ".to_string(),
        ),
        (
            "marker in body",
            "prose\n".to_string(),
            SectionId::Workflow,
            format!("<!-- TRUSTY-MPM: {token} END -->"),
        ),
        (
            "unclosed host",
            format!("<!-- TRUSTY-MPM: {token} START v=1 -->\nx\n"),
            SectionId::Workflow,
            "BODY".to_string(),
        ),
    ];

    for (label, seeded, section, body) in cases {
        let dir = project();
        seed(dir.path(), &seeded);
        let before = read(dir.path());

        write_section_override(dir.path(), section, &body)
            .expect_err(&format!("{label} must be refused"));

        assert_eq!(read(dir.path()), before, "{label} must not alter the file");
    }
}

// ---------------------------------------------------------------------------
// Behaviour 6 — the compiled-instructions pointer
// ---------------------------------------------------------------------------

/// The pointer path must stay in step with the pipeline that writes the file.
#[test]
fn pointer_path_matches_the_instruction_pipeline() {
    // Why (#4752): this constant is a SECOND spelling of a path only
    // `instruction_pipeline::compiled_prompt_path` actually produces. It already
    // went stale once — it named the global `~/.trusty-mpm/framework/...` after
    // the compiled prompt went project-local, so the pointer spliced into a
    // project's CLAUDE.md would have sent readers to a file nothing writes.
    // Nothing else fails when these two drift, because `ensure_compiled_pointer`
    // has no production caller yet (spec §10.5 defers wiring it).
    // #4832: the produced path carries a concrete session id where the pointer
    // carries the `<session-id>` placeholder, so compare with that substituted.
    let project = std::path::Path::new("/some/project");
    let produced = crate::core::instruction_pipeline::compiled_prompt_path(project, "SID");
    let relative = produced
        .strip_prefix(project)
        .expect("compiled_prompt_path must be project-local");
    assert_eq!(
        relative.to_string_lossy(),
        COMPILED_INSTRUCTIONS_PATH.replace("<session-id>", "SID"),
        "the pointer path and the pipeline that writes the file have drifted"
    );
}

/// The pointer names the compiled prompt path, visibly.
#[test]
fn pointer_block_names_the_compiled_instructions_path() {
    let dir = project();
    ensure_compiled_pointer(dir.path()).expect("pointer written");

    let text = read(dir.path());
    assert!(
        text.contains(COMPILED_INSTRUCTIONS_PATH),
        "pointer must name the compiled path, got:\n{text}"
    );
    assert!(text.contains(POINTER_BEGIN) && text.contains(POINTER_END));
}

/// The pointer must be inert to the section reader: no override, and — the part
/// that actually matters — no `unknown section token` diagnostic. A delimiter
/// spelled inside the `TRUSTY-MPM:` namespace would trip that, so this test is
/// what keeps the pointer out of the marker grammar.
#[test]
fn pointer_block_is_invisible_to_the_section_reader() {
    let dir = project();
    ensure_compiled_pointer(dir.path()).expect("pointer written");

    let scanned = scan_project(dir.path());
    assert!(
        scanned.overrides.is_empty(),
        "pointer must contribute no override: {:?}",
        scanned.overrides
    );
    assert!(
        scanned.diagnostics.is_empty(),
        "pointer must raise no diagnostic: {:?}",
        scanned.diagnostics
    );
}

/// Writing the pointer twice leaves exactly one.
#[test]
fn pointer_write_is_idempotent() {
    let dir = project();
    ensure_compiled_pointer(dir.path()).expect("first");
    let after_first = read(dir.path());

    let outcome = ensure_compiled_pointer(dir.path()).expect("second");
    assert_eq!(outcome, WriteOutcome::Unchanged);
    assert_eq!(read(dir.path()), after_first);
    assert_eq!(
        read(dir.path()).matches(POINTER_BEGIN).count(),
        1,
        "exactly one pointer block"
    );
}

/// An unpaired pointer begin marker must not make the writer treat the rest of
/// the file as the block's body.
#[test]
fn an_unpaired_pointer_marker_does_not_consume_the_file() {
    let dir = project();
    seed(dir.path(), &format!("{POINTER_BEGIN}\nstranded prose\n"));

    ensure_compiled_pointer(dir.path()).expect("write accepted");

    let text = read(dir.path());
    assert!(
        text.contains("stranded prose"),
        "content after an unpaired marker must survive, got:\n{text}"
    );
    assert!(text.contains(COMPILED_INSTRUCTIONS_PATH));
}

/// A section override and the pointer occupy independent regions.
#[test]
fn pointer_and_section_block_coexist() {
    let dir = project();
    write_section_override(dir.path(), SectionId::Workflow, "WF").expect("section write");
    ensure_compiled_pointer(dir.path()).expect("pointer write");
    write_section_override(dir.path(), SectionId::Workflow, "WF2").expect("section rewrite");

    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1, "{:?}", scanned.diagnostics);
    assert_eq!(scanned.overrides[0].body, "WF2");
    assert!(scanned.diagnostics.is_empty(), "{:?}", scanned.diagnostics);
    assert_eq!(
        read(dir.path()).matches(POINTER_BEGIN).count(),
        1,
        "the pointer must survive a later section rewrite"
    );
}

// ---------------------------------------------------------------------------
// Review follow-ups (#4762): line endings, the EOF replace path, pointer
// duplicate collapse.
// ---------------------------------------------------------------------------

/// A block spliced into a CRLF host must be CRLF too. Without this the writer
/// leaves a mixed-ending file — a user-visible defect in a file the project
/// owns. The assertion is that NO bare LF survives anywhere.
#[test]
fn matches_the_hosts_crlf_line_endings() {
    let dir = project();
    seed(dir.path(), "# Project\r\n\r\nExisting prose.\r\n");

    write_section_override(dir.path(), SectionId::Workflow, "line one\nline two")
        .expect("write accepted");

    let text = read(dir.path());
    assert!(
        !text.replace("\r\n", "").contains('\n'),
        "no bare LF may survive in a CRLF host:\n{text:?}"
    );
    assert!(text.contains("\r\n"), "the file is still CRLF");
    // And the reader still resolves the multi-line body correctly.
    assert_eq!(
        scan_project(dir.path()).overrides[0].body,
        "line one\nline two"
    );
}

/// The dominant ending wins in a mixed host, rather than the first one seen.
#[test]
fn a_mixed_ending_host_takes_the_dominant_ending() {
    let dir = project();
    // One LF line, three CRLF lines: CRLF dominates.
    seed(dir.path(), "lf-line\r\na\r\nb\r\nc\n");

    write_section_override(dir.path(), SectionId::Memory, "BODY").expect("write accepted");

    let text = read(dir.path());
    let appended = &text[text.find("TRUSTY-MPM").expect("marker present")..];
    assert!(
        !appended.replace("\r\n", "").contains('\n'),
        "the appended block must use the dominant CRLF ending:\n{appended:?}"
    );
}

/// Replacing a block that sits at end-of-file in a host with NO trailing
/// newline must not invent one. This is the `splice` EOF branch, which was
/// previously reachable only by inspection.
#[test]
fn replacing_a_trailing_block_preserves_a_missing_final_newline() {
    let dir = project();
    let token = section_token(SectionId::Workflow);
    // No trailing newline after the END marker — the block ends the file.
    seed(
        dir.path(),
        &format!(
            "intro\n<!-- TRUSTY-MPM: {token} START v=1 -->\nOLD\n<!-- TRUSTY-MPM: {token} END -->"
        ),
    );
    assert!(
        !read(dir.path()).ends_with('\n'),
        "premise: the fixture has no final newline"
    );

    write_section_override(dir.path(), SectionId::Workflow, "NEW").expect("write accepted");

    let text = read(dir.path());
    assert!(
        !text.ends_with('\n'),
        "the missing final newline must be preserved, got:\n{text:?}"
    );
    assert!(text.starts_with("intro\n"), "prefix preserved");
    let scanned = scan_project(dir.path());
    assert_eq!(scanned.overrides.len(), 1, "{:?}", scanned.diagnostics);
    assert_eq!(scanned.overrides[0].body, "NEW");
}

/// A host that already carries two pointer blocks is collapsed to one, matching
/// the section-override rule. The fixture seeds genuine duplicates, so the test
/// cannot pass unless the collapse actually happens.
#[test]
fn pointer_write_collapses_duplicate_pointer_blocks() {
    let dir = project();
    seed(
        dir.path(),
        &format!(
            "intro\n\
             {POINTER_BEGIN}\n> stale first copy\n{POINTER_END}\n\
             middle\n\
             {POINTER_BEGIN}\n> stale second copy\n{POINTER_END}\n\
             outro\n"
        ),
    );
    assert_eq!(
        read(dir.path()).matches(POINTER_BEGIN).count(),
        2,
        "premise: two pointer blocks"
    );

    ensure_compiled_pointer(dir.path()).expect("write accepted");

    let text = read(dir.path());
    assert_eq!(
        text.matches(POINTER_BEGIN).count(),
        1,
        "duplicates must be collapsed:\n{text}"
    );
    assert!(!text.contains("stale first copy"));
    assert!(!text.contains("stale second copy"));
    assert!(text.contains(COMPILED_INSTRUCTIONS_PATH));
    for kept in ["intro", "middle", "outro"] {
        assert!(text.contains(kept), "unrelated prose `{kept}` must survive");
    }
}
