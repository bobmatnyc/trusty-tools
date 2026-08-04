//! Write-side owner of a project's `CLAUDE.md` — named-section override blocks
//! and the compiled-instructions pointer (issue #4754).
//!
//! Why: [`crate::core::claude_md_sections`] is explicitly reader-only ("Scope:
//! reader only"), so every write to `CLAUDE.md` has so far been performed by
//! hand — by an LLM following prose in the `tm-init` skill — against a grammar
//! only the reader actually knows. That is the pairing that fails: prose cannot
//! be idempotent, and the repeat-application bug it invites is not hypothetical
//! in this repo. The shared `## [Unreleased]` changelog section was edited the
//! same hand-merged way and five concurrent PRs stacked five copies into it
//! (#4463–#4475, #4399 burned three rebase rounds), which is exactly why
//! changelog entries moved to per-PR fragment files. A section override stacked
//! twice is worse than a changelog stacked twice: the reader's block parser keeps the
//! FIRST well-formed block and reports the rest as
//! [`crate::core::claude_md_sections::REASON_DUPLICATE`], so the second copy is the one the
//! author just wrote and the one silently ignored.
//!
//! What: [`write_section_override`] — the single entry point that creates,
//! replaces, or leaves alone one section's marker block — and
//! [`ensure_compiled_pointer`], which records where the composed system prompt
//! actually lands. Both are idempotent by construction and both preserve every
//! byte outside the region they own.
//!
//! THE GRAMMAR LIVES IN THE READER, NOT HERE. This module never parses a marker
//! itself: it locates existing blocks with
//! [`locate_blocks`], screens bodies with
//! [`contains_marker_line`], stamps
//! [`SUPPORTED_VERSION`], and spells the token with
//! [`section_token`]. A second grammar here would drift, and
//! the first divergence would be a block this crate writes and this crate then
//! declines to read — an override that is advertised and unread, issue #381
//! verbatim.
//!
//! WHO DECIDES WHAT MAY BE WRITTEN — the package, and only the package, exactly
//! as on the read side. [`section_is_writable`] asks
//! [`CustomizationTier::permits`] for the section as the shipped package
//! declares it. `core` is refused because the package declares it `fixed`, not
//! because this module carries a list naming it; a hardcoded list here would be
//! a second source of truth, and the first time the two disagreed the protected
//! section would become writable in the one surface that must never write it.
//!
//! NEVER FAIL OPEN. Every refusal in this module leaves MORE framework
//! instruction in force, never less, and never a damaged file: a protected
//! section, an empty body, a body carrying its own marker line, a host whose
//! markers are unpaired, and an unavailable package all decline the write and
//! leave `CLAUDE.md` byte-identical. A customization mistake must never be able
//! to delete the PM's instructions — the writer's version of the reader's
//! standing rule.
//!
//! Test: `claude_md_writer_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::agent_manifest::atomic_write;
use crate::core::bundled_pm_package::bundled_fallback_package;
use crate::core::claude_md_sections::{
    HOST_FILES, SUPPORTED_VERSION, contains_marker_line, locate_blocks, section_token,
};
use crate::core::instruction_package::{CustomizationTier, OverrideTier, SectionId};

/// Where the composed PM system prompt is written, as named in a project's
/// `CLAUDE.md`.
///
/// Why: `CLAUDE.md` and the compiled prompt are read by two different readers
/// (the reader-side module's "CLAUDE.md is read twice" section), and nothing in
/// the project's own file says where the composed half ends up. A reader of
/// `CLAUDE.md` could see marker blocks with no way to find what they compile
/// into. The pointer runs one way only — `CLAUDE.md` → compiled file — so this
/// module records the path without owning, creating, or reading it.
/// What: the tilde-form path, written verbatim into the pointer block. Producing
/// the file at this path belongs to the instruction pipeline, not here.
/// Test: `pointer_block_names_the_compiled_instructions_path`.
pub const COMPILED_INSTRUCTIONS_PATH: &str = "~/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md";

/// First line of the managed compiled-instructions pointer block.
///
/// Why: the pointer is NOT a section override and must never be parsed as one.
/// These delimiters deliberately fail [`crate::core::claude_md_sections`]'s namespace test —
/// `trusty-mpm ` is not `TRUSTY-MPM:` — so the reader treats the whole block as
/// ordinary prose and raises no `unknown section token` diagnostic for it.
/// What: the begin marker; its presence is how a repeat call finds the block.
/// Test: `pointer_block_is_invisible_to_the_section_reader`.
pub const POINTER_BEGIN: &str = "<!-- trusty-mpm compiled-instructions pointer >>> -->";

/// Last line of the managed compiled-instructions pointer block.
///
/// Test: `pointer_block_is_invisible_to_the_section_reader`.
pub const POINTER_END: &str = "<!-- <<< trusty-mpm compiled-instructions pointer -->";

/// What a write did to the host file.
///
/// Why: `Unchanged` is reported separately from `Replaced` so idempotency is an
/// observable outcome rather than an inference from an unchanged mtime — a
/// second identical call is provably a no-op, not merely a rewrite that
/// happened to produce the same bytes.
/// What: one variant per terminal state of a write.
/// Test: `applying_the_same_override_twice_reports_unchanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The host did not exist and was created holding just this block.
    Created,
    /// The host existed and the block was appended to it.
    Inserted,
    /// An existing block for this section was replaced in place.
    Replaced,
    /// The host already held exactly this block; nothing was written.
    Unchanged,
}

/// Why a write was declined, leaving the host untouched.
///
/// Why: every variant is a case where the bundled section stays in force and
/// `CLAUDE.md` keeps its exact prior bytes. Returning them as data rather than
/// only logging keeps "the writer refused" an assertable fact, matching the
/// reader's [`crate::core::claude_md_sections::Rejection`] treatment.
/// What: one variant per rule in the write contract.
/// Test: one test per variant in `claude_md_writer_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteRejection {
    /// The section's declared tier admits no project-tier override.
    #[error("section {section:?} is tier {tier:?} and may not be written from CLAUDE.md")]
    Protected {
        /// The section the write aimed at.
        section: SectionId,
        /// The tier the package declares for it.
        tier: CustomizationTier,
    },
    /// The package declares no such section.
    #[error("section {section:?} is not declared by this package")]
    UnknownSection {
        /// The section the write aimed at.
        section: SectionId,
    },
    /// The body was empty once trimmed — never write a section-blanking block.
    #[error("override body for section {section:?} is empty; nothing written")]
    EmptyBody {
        /// The section the write aimed at.
        section: SectionId,
    },
    /// The body carried a marker line, which would nest and destroy the block.
    #[error("override body for section {section:?} contains a TRUSTY-MPM marker line")]
    BodyContainsMarker {
        /// The section the write aimed at.
        section: SectionId,
    },
    /// The host has an unpaired `START`; its structure is not safe to edit.
    #[error("{host} contains an unclosed TRUSTY-MPM marker; refusing to edit it")]
    HostMalformed {
        /// The host file that failed the structural check.
        host: PathBuf,
    },
    /// The bundled package could not be loaded, so no tier could be consulted.
    #[error("bundled instruction package unavailable ({0}); refusing to write")]
    PackageUnavailable(&'static str),
    /// Reading or writing the host failed.
    #[error("{0}")]
    Io(String),
}

/// The project-relative host every write targets.
///
/// Why: taken from the reader's [`HOST_FILES`] rather than spelled again here,
/// so the writer cannot target a file the reader does not scan.
/// What: the sole host path, joined onto a project root by [`host_path`].
/// Test: `writes_target_the_readers_only_host`.
fn host_path(project_dir: &Path) -> PathBuf {
    project_dir.join(HOST_FILES[0])
}

/// Confirm the shipped package permits a project-tier write of `section`.
///
/// Why: the single decision point for "may this be written?", asking the package
/// exactly as `InstructionPackage::with_overrides` does on
/// the read side. Keeping both sides on one authority is what makes the
/// protected section unwritable rather than merely undocumented.
/// What: `Ok(())` when the declared tier admits [`OverrideTier::Project`];
/// otherwise a rejection, logged at `warn` because a declined customization the
/// author never hears about is the #381 failure mode.
/// Test: `core_is_declined_and_logged`, `every_other_section_is_writable`.
fn section_is_writable(section: SectionId) -> Result<(), WriteRejection> {
    let package = bundled_fallback_package().map_err(WriteRejection::PackageUnavailable)?;
    let declared = package
        .section(section)
        .ok_or(WriteRejection::UnknownSection { section })?;
    if !declared.customization_tier.permits(OverrideTier::Project) {
        tracing::warn!(
            section = ?section,
            tier = ?declared.customization_tier,
            "declining CLAUDE.md write: section is protected; the bundled section stays in force"
        );
        return Err(WriteRejection::Protected {
            section,
            tier: declared.customization_tier,
        });
    }
    Ok(())
}

/// The line ending the host predominantly uses.
///
/// Why: a rendered block always spells `\n` internally, so splicing one into a
/// CRLF host would leave a file with mixed endings — a user-visible defect in a
/// file the project owns, and the kind of diff noise that makes a `CLAUDE.md`
/// change unreviewable. Matching the host's dominant ending keeps the writer
/// invisible in the diff.
/// What: `"\r\n"` when more than half the newlines in `text` are CRLF, else
/// `"\n"`. An empty or newline-free host yields `"\n"`.
/// Test: `matches_the_hosts_crlf_line_endings`,
/// `a_mixed_ending_host_takes_the_dominant_ending`.
fn dominant_eol(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let newlines = text.matches('\n').count();
    if crlf * 2 > newlines { "\r\n" } else { "\n" }
}

/// Re-spell a rendered block's `\n` endings as `eol`.
///
/// Why: rendering stays single-spelling (`\n`) so the templates remain readable;
/// the host-specific ending is applied once, at the boundary.
/// What: identity for `"\n"`; otherwise every `\n` becomes `\r\n`. Safe against
/// double-conversion because rendered blocks never contain a `\r`.
/// Test: `matches_the_hosts_crlf_line_endings`.
fn normalize_eol(rendered: &str, eol: &str) -> String {
    debug_assert!(!rendered.contains('\r'), "rendered blocks are LF-only");
    if eol == "\r\n" {
        rendered.replace('\n', "\r\n")
    } else {
        rendered.to_string()
    }
}

/// Render one section override block, terminator included.
///
/// Why: the emitted text has to be something the reader accepts unchanged, so
/// every variable part comes from the reader — the token from
/// [`section_token`], the version from [`SUPPORTED_VERSION`].
/// What: `START` line, trimmed body, `END` line, each `\n`-terminated; the
/// caller re-spells the endings via [`normalize_eol`] to match the host.
/// Test: `written_block_declares_the_readers_supported_version`,
/// `round_trips_through_the_reader`.
fn render_block(section: SectionId, body: &str) -> String {
    let token = section_token(section);
    format!(
        "<!-- TRUSTY-MPM: {token} START v={SUPPORTED_VERSION} -->\n{}\n<!-- TRUSTY-MPM: {token} END -->\n",
        body.trim()
    )
}

/// Render the managed compiled-instructions pointer block, terminator included.
///
/// Why: the body is visible Markdown rather than an HTML comment, because the
/// point is that a HUMAN reading `CLAUDE.md` can find the compiled prompt. Only
/// the delimiters are comments.
/// What: begin marker, a blockquote naming [`COMPILED_INSTRUCTIONS_PATH`], end
/// marker.
/// Test: `pointer_block_names_the_compiled_instructions_path`.
fn render_pointer() -> String {
    format!(
        "{POINTER_BEGIN}\n\
         > The PM system prompt in force for this project is composed at session\n\
         > launch and written to `{COMPILED_INSTRUCTIONS_PATH}`.\n\
         > Marked `TRUSTY-MPM:` blocks in this file are inputs to it; all other\n\
         > prose here is loaded directly by Claude Code and is not copied into it.\n\
         {POINTER_END}\n"
    )
}

/// Splice `replacement` over the line spans `spans`, keeping the first and
/// dropping the rest.
///
/// Why: replacing the first span and DELETING any later duplicate is what makes
/// the post-condition "exactly one block for this section" hold. It is also
/// semantically free: the reader already keeps the first block and reports the
/// others as [`crate::core::claude_md_sections::REASON_DUPLICATE`], so removing them changes
/// which bytes are on disk but not which override is in force. Leaving them
/// would mean a caller could read back its own freshly written value and get a
/// stale one — the stacking failure this module exists to prevent.
///
/// Line indices come from the reader's parse of the same string, and
/// `split_inclusive` yields one segment per `lines()` item while retaining each
/// terminator, so untouched regions survive byte-for-byte including `\r\n` and
/// trailing whitespace.
/// What: concatenates the segments outside every span, emitting `replacement` at
/// the first. Restores a missing final newline when the original lacked one and
/// the last span reached end-of-file.
/// Test: `preserves_surrounding_content_byte_for_byte`,
/// `replacing_collapses_duplicate_blocks_to_one`,
/// `preserves_crlf_line_endings`.
fn splice(text: &str, spans: &[(usize, usize)], replacement: &str) -> String {
    let segments: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len() + replacement.len());
    let mut cursor = 0usize;

    for (index, (start, end)) in spans.iter().enumerate() {
        out.push_str(&segments[cursor..*start].concat());
        if index == 0 {
            out.push_str(replacement);
        }
        cursor = end + 1;
    }
    let consumed_to_eof = cursor >= segments.len();
    out.push_str(&segments[cursor..].concat());

    if consumed_to_eof && !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Append `block` to `text`, separated by one blank line.
///
/// Why: a project's own prose and a managed block must stay visually distinct,
/// and an existing file that lacks a final newline would otherwise have its last
/// line swallowed into the marker line.
/// What: returns `block` alone for empty input; otherwise `text`, a normalising
/// line ending when absent, a blank line, then `block` — both separators spelled
/// with the host's own `eol`.
/// Test: `appends_after_existing_prose`,
/// `appends_newline_when_existing_file_lacks_trailing_newline`,
/// `matches_the_hosts_crlf_line_endings`.
fn append_block(text: &str, block: &str, eol: &str) -> String {
    if text.is_empty() {
        return block.to_string();
    }
    let mut out = String::with_capacity(text.len() + block.len() + 2 * eol.len());
    out.push_str(text);
    if !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(eol);
    out.push_str(block);
    out
}

/// Read the host, treating absence as empty content.
///
/// Why: "no `CLAUDE.md` yet" is the ordinary first-write case, not an error, and
/// must not be distinguishable from an empty one by anything but the outcome.
/// What: `Ok((contents, existed))`; a genuine IO error is surfaced rather than
/// swallowed, because writing over a file that could not be read risks
/// destroying content this module promised to preserve.
/// Test: `creates_the_host_when_absent`, `unreadable_host_is_reported_not_clobbered`.
fn read_host(path: &Path) -> Result<(String, bool), WriteRejection> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok((text, true)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), false)),
        Err(err) => Err(WriteRejection::Io(format!("{}: {err}", path.display()))),
    }
}

/// Persist `contents` to `path` unless it already matches.
///
/// Why: routed through the shared [`atomic_write`] entry point (the common
/// entry-point rule) so a crash mid-write cannot leave a half-written
/// `CLAUDE.md` — the file whose corruption would cost a project its
/// instructions.
/// What: writes via temp-file rename; returns `chosen` unchanged for the caller
/// to report.
/// Test: covered by every write test's on-disk assertion.
fn persist(
    path: &Path,
    contents: &str,
    chosen: WriteOutcome,
) -> Result<WriteOutcome, WriteRejection> {
    atomic_write(path, contents).map_err(|err| WriteRejection::Io(err.to_string()))?;
    Ok(chosen)
}

/// Create, replace, or confirm one named-section override block in a project's
/// `CLAUDE.md`.
///
/// Why: the single owner of section-override writes. Every caller routes here so
/// that idempotency, the protected-section refusal, and byte preservation are
/// properties of the system rather than of whoever wrote the calling prose.
///
/// What, in order: the package must permit the section; the body must be
/// non-empty once trimmed and must not contain a marker line; the host's markers
/// must all be paired. Only then does it splice. An existing block for the
/// section is replaced in place and any duplicate of it removed, so exactly one
/// block for that section survives; with no existing block, one is appended.
/// Re-running with an identical body writes nothing and reports
/// [`WriteOutcome::Unchanged`].
///
/// Every refusal leaves the file byte-identical and the bundled section in
/// force.
///
/// Test: `writes_a_new_block_a_reader_accepts`,
/// `applying_the_same_override_twice_reports_unchanged`,
/// `applying_twice_leaves_exactly_one_block`, `core_is_declined_and_logged`,
/// `preserves_surrounding_content_byte_for_byte`,
/// `an_unclosed_marker_blocks_the_write`.
pub fn write_section_override(
    project_dir: &Path,
    section: SectionId,
    body: &str,
) -> Result<WriteOutcome, WriteRejection> {
    section_is_writable(section)?;
    if body.trim().is_empty() {
        return Err(WriteRejection::EmptyBody { section });
    }
    if contains_marker_line(body) {
        return Err(WriteRejection::BodyContainsMarker { section });
    }

    let path = host_path(project_dir);
    let (text, existed) = read_host(&path)?;

    let located = locate_blocks(&text, section);
    if located.unclosed {
        tracing::warn!(
            path = %path.display(),
            "declining CLAUDE.md write: host has an unclosed TRUSTY-MPM marker"
        );
        return Err(WriteRejection::HostMalformed { host: path });
    }

    let eol = dominant_eol(&text);
    let block = normalize_eol(&render_block(section, body), eol);
    let next = if located.spans.is_empty() {
        append_block(&text, &block, eol)
    } else {
        splice(&text, &located.spans, &block)
    };

    if existed && next == text {
        return Ok(WriteOutcome::Unchanged);
    }
    let outcome = match (existed, located.spans.is_empty()) {
        (false, _) => WriteOutcome::Created,
        (true, true) => WriteOutcome::Inserted,
        (true, false) => WriteOutcome::Replaced,
    };
    persist(&path, &next, outcome)
}

/// Ensure the project's `CLAUDE.md` names where its compiled prompt lands.
///
/// Why: behaviour 6 of #4754 — a reader of `CLAUDE.md` should be able to find
/// the composed system prompt actually in force. The pointer is one-directional
/// and inert: this module writes the path and never reads, creates, or validates
/// the file it names, which stays the instruction pipeline's to produce.
/// What: appends the managed pointer block, or replaces it when its content has
/// changed; reports [`WriteOutcome::Unchanged`] when already correct. The block
/// is delimited by comments the section reader does not recognise, so it
/// contributes no override and no diagnostic.
/// Test: `pointer_block_names_the_compiled_instructions_path`,
/// `pointer_write_is_idempotent`, `pointer_block_is_invisible_to_the_section_reader`.
pub fn ensure_compiled_pointer(project_dir: &Path) -> Result<WriteOutcome, WriteRejection> {
    let path = host_path(project_dir);
    let (text, existed) = read_host(&path)?;
    let eol = dominant_eol(&text);
    let block = normalize_eol(&render_pointer(), eol);

    let spans = pointer_spans(&text);
    let next = if spans.is_empty() {
        append_block(&text, &block, eol)
    } else {
        splice(&text, &spans, &block)
    };

    if existed && next == text {
        return Ok(WriteOutcome::Unchanged);
    }
    let outcome = match (existed, spans.is_empty()) {
        (false, _) => WriteOutcome::Created,
        (true, true) => WriteOutcome::Inserted,
        (true, false) => WriteOutcome::Replaced,
    };
    persist(&path, &next, outcome)
}

/// Locate the managed pointer block's line span, if present.
///
/// Why: the pointer is deliberately outside the marker grammar, so
/// [`locate_blocks`] cannot find it and this small paired scan is required. It
/// stays conservative in the same direction as the reader: an unpaired begin
/// marker yields no span, so the block is appended rather than the file being
/// rewritten around a delimiter whose extent is unknown.
/// It also returns EVERY paired block, not just the first, so [`splice`]
/// collapses duplicates here exactly as it does for sections. The argument is
/// the same one that justified collapsing there: a caller must be able to read
/// back what it wrote, and a second pointer block left behind is a stale copy a
/// reader can find first.
/// What: one `(start, end)` span per paired delimiter, in file order, matching
/// trimmed delimiter lines.
/// Test: `pointer_write_is_idempotent`,
/// `an_unpaired_pointer_marker_does_not_consume_the_file`,
/// `pointer_write_collapses_duplicate_pointer_blocks`.
fn pointer_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == POINTER_BEGIN {
            start = Some(index);
        } else if trimmed == POINTER_END
            && let Some(open) = start.take()
        {
            spans.push((open, index));
        }
    }
    spans
}

// A `remove_section_override` counterpart is deliberately absent. Removing a
// block means reverting to the bundled section, which the reader already does
// for an ABSENT block — so deletion has a safe spelling a caller can reach
// without a code path whose bug mode is "deleted more of CLAUDE.md than asked".
// Add it only with a caller that needs it, per issue #4754's minimalism note.

#[cfg(test)]
#[path = "claude_md_writer_tests.rs"]
mod tests;
