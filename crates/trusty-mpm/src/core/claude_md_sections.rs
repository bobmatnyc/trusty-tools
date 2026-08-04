//! Named-section PM instruction overrides read from the project's `CLAUDE.md`.
//!
//! Why: project customization of the PM prompt is spread across five separate
//! files under `<project>/.trusty-mpm/` ([`crate::core::instruction_overrides`]),
//! which forces a project to maintain a second instruction surface next to the
//! `CLAUDE.md` it already has, and gives each override file whole-document
//! granularity. Epic #4183 collapses that onto ONE surface: a named section
//! marked out inside `CLAUDE.md` overrides exactly the matching
//! [`SectionId`] of the bundled instruction package, and nothing else.
//!
//! This module is the READER, and it ships **before** the floor text that
//! advertises the mechanism. Shipping the advertisement first is issue #381
//! verbatim — `BASE_PM.md` told the PM it could drop override files into
//! `.trusty-mpm/` for two years while no code read them, so every advertised
//! override was a silent no-op. An override mechanism must be readable before it
//! is documented, never after.
//!
//! What: the marker grammar, the host search list, and
//! [`InstructionPackage::with_overrides`] — the single point that decides whether
//! a parsed override may replace a section. The grammar, matched whole-line:
//!
//! ```text
//! <!-- TRUSTY-MPM: WORKFLOW START v=1 -->
//! …override content, verbatim…
//! <!-- TRUSTY-MPM: WORKFLOW END -->
//! ```
//!
//! The section token is the [`SectionId`] kebab-case name uppercased (`CORE`,
//! `MEMORY`, `SEARCH`, `WORKFLOW`, `AGENT-DELEGATION`, and the four floor
//! tokens), matched case-insensitively. Content is what lies strictly between
//! the two marker lines, trimmed. Text outside markers is not instruction
//! content and is ignored.
//!
//! Hosts are scanned in [`HOST_FILES`] order and `CLAUDE.md` wins a same-section
//! collision, because it is the surface #4183 is moving customization TO.
//!
//! WHO DECIDES WHAT IS OVERRIDABLE — the package, and only the package. This
//! module never carries a list of overridable sections; it asks
//! [`crate::core::instruction_package::CustomizationTier::permits`] for the
//! section as the shipped package declares it. A second list here would be a
//! second source of truth, and the first time the two disagreed the floor would
//! become overridable in exactly the surface that must never be.
//!
//! NEVER FAIL CLOSED. Every failure in this module degrades toward MORE
//! framework instruction, never toward an aborted launch or a blank section: an
//! absent or unreadable host, a `START` with no `END`, an unknown token, an
//! unknown `v=`, an override aimed at the floor, an empty body, and a package
//! that no longer validates all resolve to "keep the bundled section". A
//! customization mistake must never be able to delete the PM's instructions.
//!
//! Scope: reader only. The five `.trusty-mpm/` override file constants stay
//! where they are and keep working; the floor text that advertises named
//! sections, and the removal of the legacy files, are separate steps of #4183 /
//! #4286.
//!
//! Test: `claude_md_sections_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::instruction_package::{
    BlockBody, CustomizationTier, InstructionPackage, OverrideTier, SectionId, ValidationError,
};

/// Project-relative files scanned for marker blocks, highest precedence first.
///
/// Why: `CLAUDE.md` is the SOLE project customization surface (#4286).
/// `.trusty-mpm/INSTRUCTIONS.md` was a second host while that file was still an
/// additive override; keeping it after the read path retired would leave the
/// file half-alive — honoured for marked blocks, ignored for everything else —
/// which is precisely the ambiguous state the retirement removes. Dropping it
/// costs nothing measurable: a survey of all 52 `.trusty-mpm/INSTRUCTIONS.md`
/// instances on the development machine found ZERO containing a `TRUSTY-MPM:`
/// marker, so no project loses a named override by this change.
///
/// The list stays an array, and [`scan_project`] keeps its cross-host
/// precedence handling ([`REASON_SHADOWED`]), because the host set is DATA: that
/// branch is what makes adding a host back a one-line change rather than a
/// correctness question.
/// What: the single host path, joined onto the project root.
/// Test: `claude_md_is_the_only_marker_host`,
/// `a_marker_in_the_retired_instructions_file_is_not_read`.
pub const HOST_FILES: [&str; 1] = ["CLAUDE.md"];

/// The only marker version this build understands.
///
/// Why: [`crate::core::claude_md_writer`] stamps this exact number into every
/// block it emits rather than hardcoding its own `1`. A writer that spelled the
/// version independently could drift past the reader on the next bump and emit
/// blocks its own reader would decline as [`REASON_UNSUPPORTED_VERSION`] — the
/// writer/reader drift this pairing exists to make impossible.
/// What: the `v=` value written, and the only one accepted.
/// Test: `written_block_declares_the_readers_supported_version`.
pub(crate) const SUPPORTED_VERSION: u32 = 1;

/// Literal opening a marker line.
const MARKER_OPEN: &str = "<!--";
/// Literal closing a marker line.
const MARKER_CLOSE: &str = "-->";
/// Namespace that distinguishes a framework marker from an ordinary comment.
const MARKER_NAMESPACE: &str = "TRUSTY-MPM:";

/// A marker block was opened and never closed (or was displaced by a nested one).
pub const REASON_UNCLOSED: &str = "START marker has no matching END; block skipped";
/// An `END` marker named a different section than the open `START`.
pub const REASON_MISMATCHED_END: &str = "END marker names a different section than the open START";
/// An `END` marker appeared with no block open.
pub const REASON_UNMATCHED_END: &str = "END marker with no open START";
/// The section token is not a known [`SectionId`].
pub const REASON_UNKNOWN_SECTION: &str = "unknown section token; block skipped";
/// The `v=` field named a version this build does not implement.
pub const REASON_UNSUPPORTED_VERSION: &str = "unsupported marker version; block skipped";
/// The `v=` field was absent; treated as `v=1` for this release.
pub const REASON_MISSING_VERSION: &str = "marker has no `v=`; assuming v=1";
/// The body was empty once trimmed — never blank a section over it.
pub const REASON_EMPTY_BODY: &str = "override body is empty; keeping the bundled section";
/// A later block in the same host repeated a section already claimed.
pub const REASON_DUPLICATE: &str = "duplicate section marker; the first well-formed block wins";
/// A lower-precedence host repeated a section a higher-precedence host claimed.
pub const REASON_SHADOWED: &str = "section already overridden by a higher-precedence host";

/// The marker token for a section: its kebab-case name, uppercased.
///
/// Why: the token has to be stable and human-typable, and it must not drift from
/// the enum. Deriving it from the serde name by hand here — and pinning that
/// correspondence in a test — keeps a renamed section from silently orphaning
/// every project's marker.
/// What: the nine tokens, matched case-insensitively by [`section_for_token`].
/// Test: `every_section_token_is_the_kebab_case_id_uppercased`.
pub const fn section_token(id: SectionId) -> &'static str {
    match id {
        SectionId::Identity => "IDENTITY",
        SectionId::Core => "CORE",
        SectionId::Memory => "MEMORY",
        SectionId::Search => "SEARCH",
        SectionId::Workflow => "WORKFLOW",
        SectionId::AgentDelegation => "AGENT-DELEGATION",
        SectionId::Enforcement => "ENFORCEMENT",
        SectionId::NonOverridableRules => "NON-OVERRIDABLE-RULES",
        SectionId::FrameworkGuaranteedConventions => "FRAMEWORK-GUARANTEED-CONVENTIONS",
    }
}

/// Resolve a marker token to its section, case-insensitively.
///
/// Test: `unknown_section_token_is_skipped_with_a_diagnostic`.
fn section_for_token(token: &str) -> Option<SectionId> {
    SectionId::CANONICAL
        .into_iter()
        .find(|id| token.eq_ignore_ascii_case(section_token(*id)))
}

/// How a `START` marker declared its format version.
///
/// Why: "absent" and "unrecognised" must not collapse into one state. A missing
/// `v=` is an authoring slip on a mechanism that has exactly one version so far,
/// and rejecting the block for it would blank a section the author expected to
/// keep; an unrecognised `v=` means the block was written for a format this
/// build does not implement, and applying it anyway would deliver instructions
/// nobody wrote.
/// What: absent (assume [`SUPPORTED_VERSION`], warn), supported, unsupported.
/// Test: `missing_version_is_accepted_as_v1_with_a_diagnostic`,
/// `unsupported_version_is_skipped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerVersion {
    /// No `v=` field at all.
    Absent,
    /// `v=1`.
    Supported,
    /// A `v=` this build does not implement, or an unparseable one.
    Unsupported,
}

/// Which half of a pair a marker line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    /// Opens a block, carrying its declared version.
    Start(MarkerVersion),
    /// Closes a block.
    End,
}

/// One recognised marker line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    /// The section token exactly as written (case preserved for diagnostics).
    token: String,
    /// Whether it opens or closes a block.
    kind: MarkerKind,
}

/// Parse one line as a framework marker, or `None` if it is ordinary text.
///
/// Why: recognition must be conservative in one direction and liberal in the
/// other. A line that is not a `TRUSTY-MPM:` comment must NEVER be treated as a
/// marker (prose would start disappearing into override blocks), while a line
/// that is clearly one — but malformed — must still be recognised, so its
/// partner does not dangle and corrupt the surrounding blocks.
/// What: whole-line match on `<!-- TRUSTY-MPM: <TOKEN> START|END [v=N] -->`,
/// with surrounding and interior whitespace free-form and every keyword matched
/// case-insensitively. A trailing field other than `v=…`, or any extra field,
/// makes the line ordinary text.
/// Test: `parses_the_documented_marker_grammar`,
/// `prose_mentioning_the_marker_is_not_a_marker`.
fn parse_marker(line: &str) -> Option<Marker> {
    let body = line
        .trim()
        .strip_prefix(MARKER_OPEN)?
        .strip_suffix(MARKER_CLOSE)?
        .trim();
    let namespaced = body
        .get(..MARKER_NAMESPACE.len())
        .filter(|head| head.eq_ignore_ascii_case(MARKER_NAMESPACE))?;
    let mut fields = body[namespaced.len()..].split_whitespace();

    let token = fields.next()?.to_string();
    let kind = match fields.next()? {
        word if word.eq_ignore_ascii_case("START") => MarkerKind::Start(match fields.next() {
            None => MarkerVersion::Absent,
            Some(field) => match field.get(..2).filter(|k| k.eq_ignore_ascii_case("v=")) {
                Some(_) if field[2..].parse::<u32>() == Ok(SUPPORTED_VERSION) => {
                    MarkerVersion::Supported
                }
                _ => MarkerVersion::Unsupported,
            },
        }),
        word if word.eq_ignore_ascii_case("END") => MarkerKind::End,
        _ => return None,
    };
    if fields.next().is_some() {
        return None;
    }
    Some(Marker { token, kind })
}

/// A `START`/`END` pair found in a host file, before any semantic filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBlock {
    /// The section the token resolves to, if it resolves at all.
    section: Option<SectionId>,
    /// The declared format version.
    version: MarkerVersion,
    /// Content strictly between the marker lines, trimmed.
    body: String,
    /// 0-based index of the `START` line.
    start_line: usize,
    /// 0-based index of the `END` line.
    end_line: usize,
}

/// Why one parsed block was not applied.
///
/// Why: these are returned as data rather than only logged, because a warning
/// that exists only in a `tracing` subscriber cannot be asserted, and this
/// module's entire contract is about what it declines to do.
/// What: the host, the 1-based line of the marker at fault, and one of the
/// `REASON_*` constants.
/// Test: every `*_is_skipped*` test asserts on these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanDiagnostic {
    /// The host file the marker was read from.
    pub host: PathBuf,
    /// 1-based line number of the marker at fault.
    pub line: usize,
    /// One of the `REASON_*` constants.
    pub reason: &'static str,
}

/// Build a diagnostic from a 0-based line index.
fn diagnostic(host: &Path, line_index: usize, reason: &'static str) -> ScanDiagnostic {
    ScanDiagnostic {
        host: host.to_path_buf(),
        line: line_index + 1,
        reason,
    }
}

/// An accepted, well-formed override of one section.
///
/// Test: `scans_claude_md_for_named_sections`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionOverride {
    /// The section this replaces.
    pub section: SectionId,
    /// The replacement text, trimmed and never empty.
    pub body: String,
    /// The host file it was authored in.
    pub host: PathBuf,
    /// 1-based line of its `START` marker.
    pub line: usize,
}

/// Everything a project's host files declared, accepted and rejected alike.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectOverrides {
    /// Accepted overrides, in [`SectionId`] canonical order.
    pub overrides: Vec<SectionOverride>,
    /// Blocks that were recognised but not applied.
    pub diagnostics: Vec<ScanDiagnostic>,
}

impl ProjectOverrides {
    /// Emit the operator-visible record of what was read and what was declined.
    ///
    /// Why: a silently ignored override is the failure this whole module exists
    /// to avoid repeating (#381). Every decline gets a line naming the file, the
    /// line number and the reason.
    /// What: `warn!` per diagnostic, `info!` per applied override.
    /// Test: covered indirectly; the assertable form is the returned data.
    pub fn log(&self) {
        for diagnostic in &self.diagnostics {
            tracing::warn!(
                path = %diagnostic.host.display(),
                line = diagnostic.line,
                reason = diagnostic.reason,
                "CLAUDE.md named-section override ignored"
            );
        }
        for applied in &self.overrides {
            tracing::info!(
                path = %applied.host.display(),
                section = ?applied.section,
                "applying CLAUDE.md named-section override"
            );
        }
    }
}

/// Find every well-formed marker pair in `text`, plus what went wrong.
///
/// Why: pairing is a single left-to-right pass with one open slot, so a
/// malformed region can cost at most the block it belongs to. Nesting is not
/// supported and is not silently tolerated: a `START` while a block is open
/// discards the outer block with [`REASON_UNCLOSED`] and opens the inner one, so
/// the author sees the outer disappear rather than getting a block whose body
/// silently swallowed another marker.
/// What: an ordered list of pairs and an ordered list of diagnostics.
/// Test: `nested_start_discards_the_outer_block`,
/// `unterminated_block_is_skipped_and_later_blocks_still_apply`.
fn parse_blocks(text: &str, host: &Path) -> (Vec<ParsedBlock>, Vec<ScanDiagnostic>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut diagnostics = Vec::new();
    let mut open: Option<(Marker, usize)> = None;

    for (index, line) in lines.iter().enumerate() {
        let Some(marker) = parse_marker(line) else {
            continue;
        };
        match marker.kind {
            MarkerKind::Start(_) => {
                if let Some((_, previous)) = open.replace((marker, index)) {
                    diagnostics.push(diagnostic(host, previous, REASON_UNCLOSED));
                }
            }
            MarkerKind::End => {
                let Some((start, start_line)) = open.take() else {
                    diagnostics.push(diagnostic(host, index, REASON_UNMATCHED_END));
                    continue;
                };
                if !start.token.eq_ignore_ascii_case(&marker.token) {
                    diagnostics.push(diagnostic(host, index, REASON_MISMATCHED_END));
                    continue;
                }
                let MarkerKind::Start(version) = start.kind else {
                    unreachable!("only a START marker is ever stored as open");
                };
                blocks.push(ParsedBlock {
                    section: section_for_token(&start.token),
                    version,
                    body: lines[start_line + 1..index].join("\n").trim().to_string(),
                    start_line,
                    end_line: index,
                });
            }
        }
    }

    if let Some((_, start_line)) = open {
        diagnostics.push(diagnostic(host, start_line, REASON_UNCLOSED));
    }
    (blocks, diagnostics)
}

/// Whether any line of `text` would be recognised as a framework marker.
///
/// Why: [`crate::core::claude_md_writer`] must refuse a body that itself
/// contains a marker line. Emitting one would nest a `START` inside the block
/// being written, and [`parse_blocks`] resolves nesting by DISCARDING the outer
/// block ([`REASON_UNCLOSED`]) — so an override body carrying a stray marker
/// would silently delete the very section the author was trying to set. The
/// check has to use the reader's own recogniser, not a substring search for
/// `TRUSTY-MPM:`, because prose mentioning the marker inline is legal content
/// and must stay writable.
/// What: `true` when [`parse_marker`] accepts any line of `text`.
/// Test: `body_containing_a_marker_line_is_refused`,
/// `body_mentioning_the_marker_in_prose_is_written`.
pub(crate) fn contains_marker_line(text: &str) -> bool {
    text.lines().any(|line| parse_marker(line).is_some())
}

/// Where a section's well-formed blocks sit in a host, and whether the host's
/// marker structure is intact.
///
/// Why: the writer needs exactly two facts before it edits a file — which line
/// ranges belong to the section it is replacing, and whether any marker in the
/// file is unpaired. It must not re-derive either from its own parser.
/// What: `spans` holds `(start_line, end_line)` 0-based inclusive line indices
/// of every well-formed block claiming the section, in file order; `unclosed`
/// is true when any `START` in the host lacked a matching `END`.
/// Test: `locate_blocks_finds_one_span_per_block`,
/// `locate_blocks_flags_an_unclosed_marker`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostBlocks {
    /// 0-based inclusive `(start_line, end_line)` of each matching block.
    pub(crate) spans: Vec<(usize, usize)>,
    /// Whether the host contains an unpaired `START` marker.
    pub(crate) unclosed: bool,
}

/// Locate every well-formed block in `text` that claims `section`.
///
/// Why: one grammar, one pairing pass. The writer splices by line index, and
/// deriving those indices from [`parse_blocks`] — the same function the reader
/// uses — is what guarantees the writer can never target a region the reader
/// would parse differently.
/// What: runs [`parse_blocks`] and projects the blocks whose token resolved to
/// `section`, plus whether any [`REASON_UNCLOSED`] diagnostic was raised.
/// Test: `locate_blocks_finds_one_span_per_block`,
/// `locate_blocks_flags_an_unclosed_marker`, `locate_blocks_ignores_other_sections`.
pub(crate) fn locate_blocks(text: &str, section: SectionId) -> HostBlocks {
    let (blocks, diagnostics) = parse_blocks(text, Path::new("<in-memory>"));
    HostBlocks {
        spans: blocks
            .iter()
            .filter(|b| b.section == Some(section))
            .map(|b| (b.start_line, b.end_line))
            .collect(),
        unclosed: diagnostics.iter().any(|d| d.reason == REASON_UNCLOSED),
    }
}

/// Decide whether a parsed pair may become an override.
///
/// What: an unsupported `v=` loses first (the block was written for a format
/// this build cannot honour), then an unknown token, then an empty body — which
/// is treated as ABSENT rather than as "blank this section", because the one
/// outcome no customization mistake may produce is a PM with no instructions.
/// Test: `unsupported_version_is_skipped`, `empty_body_never_blanks_a_section`.
fn accept(block: &ParsedBlock) -> Result<SectionId, &'static str> {
    if block.version == MarkerVersion::Unsupported {
        return Err(REASON_UNSUPPORTED_VERSION);
    }
    let section = block.section.ok_or(REASON_UNKNOWN_SECTION)?;
    if block.body.is_empty() {
        return Err(REASON_EMPTY_BODY);
    }
    Ok(section)
}

/// Read a host file, or `None` when it is absent or unreadable.
///
/// Why: neither absence nor an IO error may fail a launch — both mean "this
/// project declared no overrides".
/// Test: `missing_claude_md_yields_no_overrides`, `unreadable_host_is_not_fatal`.
fn read_host(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                %err,
                "instruction override host unreadable; using bundled sections"
            );
            None
        }
    }
}

/// Scan a project's host files for named-section overrides.
///
/// Why: one entry point so no caller can invent its own host list or precedence.
/// What: scans [`HOST_FILES`] in order; the first host to claim a section keeps
/// it and any later claim is reported as [`REASON_SHADOWED`] (across hosts) or
/// [`REASON_DUPLICATE`] (within one host). The result is sorted into
/// [`SectionId`] canonical order so downstream application is deterministic.
/// Test: `scans_claude_md_for_named_sections`, `claude_md_is_the_only_marker_host`,
/// `a_marker_in_the_retired_instructions_file_is_not_read`,
/// `duplicate_section_in_one_host_keeps_the_first`.
pub fn scan_project(project_dir: &Path) -> ProjectOverrides {
    let mut result = ProjectOverrides::default();

    for relative in HOST_FILES {
        let host = project_dir.join(relative);
        let Some(text) = read_host(&host) else {
            continue;
        };
        let (blocks, diagnostics) = parse_blocks(&text, &host);
        result.diagnostics.extend(diagnostics);

        for block in blocks {
            let section = match accept(&block) {
                Ok(section) => section,
                Err(reason) => {
                    result
                        .diagnostics
                        .push(diagnostic(&host, block.start_line, reason));
                    continue;
                }
            };
            if block.version == MarkerVersion::Absent {
                result.diagnostics.push(diagnostic(
                    &host,
                    block.start_line,
                    REASON_MISSING_VERSION,
                ));
            }
            if let Some(claimed) = result.overrides.iter().find(|o| o.section == section) {
                let reason = if claimed.host == host {
                    REASON_DUPLICATE
                } else {
                    REASON_SHADOWED
                };
                result
                    .diagnostics
                    .push(diagnostic(&host, block.start_line, reason));
                continue;
            }
            result.overrides.push(SectionOverride {
                section,
                body: block.body,
                host: host.clone(),
                line: block.start_line + 1,
            });
        }
    }

    result.overrides.sort_by_key(|o| o.section);
    result
}

// `strip_marker_blocks` lived here until #4286. It de-duplicated a marked block
// that appeared in `.trusty-mpm/INSTRUCTIONS.md`, which was simultaneously a
// marker host and the source of the additive `project-addendum`. Retiring that
// file removed both roles at once, leaving the function with no caller — it is
// deleted rather than kept as an unreachable helper.

/// Report overrides that a non-package composition path cannot honour.
///
/// Why: the sectioned package is the only composer that HAS sections, so a
/// project still carrying a legacy `.trusty-mpm/` override file (which forces
/// the legacy string assembly) would have its named sections quietly dropped.
/// Quietly is the part that is unacceptable — that is #381 again. The removal of
/// the legacy files is a later step of #4183; until then this says so, loudly,
/// once per override.
///
/// #4399: the legacy string assembly is NOT entirely sectionless — `WORKFLOW`,
/// `MEMORY` and `AGENT-DELEGATION` are independently addressable there too, so
/// the caller ([`crate::core::instruction_overrides::resolve_pm_prompt_with_roster`])
/// applies a named override for any of those three sections that has no
/// same-section legacy file shadowing it, and passes THIS function only the
/// overrides that genuinely could not land — a same-section legacy-file
/// collision, or `IDENTITY`/`CORE`/`SEARCH`, which have no slot at all on this
/// path. This function itself is unchanged: it still warns, unconditionally,
/// about every override in the slice it is given.
///
/// What: one `warn!` per accepted override, naming the file and the section.
/// Test: `unaddressable_sections_are_reported_unapplied_on_the_roster_absent_path`,
/// `a_legacy_file_cannot_shadow_a_named_override_because_it_is_not_read`.
pub fn warn_unapplied(scanned: &ProjectOverrides) {
    for applied in &scanned.overrides {
        tracing::warn!(
            path = %applied.host.display(),
            section = ?applied.section,
            "named-section override NOT applied: this project still uses a legacy \
             .trusty-mpm/ override file, which composes the prompt without sections"
        );
    }
}

/// Why one override was declined by [`InstructionPackage::with_overrides`].
///
/// Why: every variant is a case where the override is dropped and the BUNDLED
/// section is kept — the fail-open direction. Returning them as data rather than
/// swallowing them keeps "the floor refused the override" an assertable fact
/// instead of a claim in a doc comment.
/// What: one variant per rule in the application contract.
/// Test: one test per variant in `claude_md_sections_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// The section's declared tier admits no project-tier override.
    #[error("section {section:?} is tier {tier:?} and admits no project override")]
    NotOverridable {
        /// The section the override aimed at.
        section: SectionId,
        /// The tier the package declares for it.
        tier: CustomizationTier,
    },
    /// The package declares no such section.
    #[error("section {section:?} is not declared by this package")]
    UnknownSection {
        /// The section the override aimed at.
        section: SectionId,
    },
    /// The section owns no authored text block to replace.
    #[error("section {section:?} has no authored text block for an override to replace")]
    NoTextBlock {
        /// The section the override aimed at.
        section: SectionId,
    },
    /// The override body was empty.
    #[error("override for section {section:?} is empty; keeping the bundled section")]
    EmptyBody {
        /// The section the override aimed at.
        section: SectionId,
    },
    /// Applying it would leave the section with nothing guaranteed to emit.
    #[error("overriding section {section:?} would leave it able to emit nothing")]
    WouldEmitNothing {
        /// The section the override aimed at.
        section: SectionId,
    },
    /// Applying it produced a package that no longer validates.
    #[error("overriding section {section:?} invalidates the package: {error}")]
    Invalidates {
        /// The section the override aimed at.
        section: SectionId,
        /// Why the resulting package was rejected.
        error: ValidationError,
    },
    /// The fully overridden package failed the final validation; all reverted.
    #[error("the overridden package failed validation ({0}); composing the bundled package")]
    PackageInvalid(ValidationError),
}

/// Apply one override to a package, or say why it may not be applied.
///
/// What: replaces the section's FIRST *authored* block — [`BlockBody::Text`] or,
/// since schema v2, [`BlockBody::File`] — with the override body and drops that
/// section's remaining authored blocks, leaving every [`BlockBody::Generated`]
/// block untouched. That asymmetry is the point: a generated block is
/// host-computed content the project cannot author, so an `AGENT-DELEGATION`
/// override rewrites the routing doctrine but CANNOT suppress the live agent
/// roster (#4196 in override form).
///
/// Matching on "authored" rather than on `Text` alone is load-bearing after
/// #4318: every bundled section is now a `file` body, so a `Text`-only match would
/// have made every section silently non-overridable — `NoTextBlock` for the whole
/// taxonomy, an advertised-but-unread override, issue #381 verbatim.
/// Test: `agent_delegation_override_keeps_the_generated_roster`,
/// `floor_sections_refuse_every_named_section_override`,
/// `content_sections_accept_a_project_override`.
fn apply_one(
    package: &InstructionPackage,
    over: &SectionOverride,
) -> Result<InstructionPackage, Rejection> {
    let section = over.section;
    let body = over.body.trim();
    if body.is_empty() {
        return Err(Rejection::EmptyBody { section });
    }
    let Some(declared) = package.section(section) else {
        return Err(Rejection::UnknownSection { section });
    };
    if !declared.customization_tier.permits(OverrideTier::Project) {
        return Err(Rejection::NotOverridable {
            section,
            tier: declared.customization_tier,
        });
    }

    let mut next = package.clone();
    let mut replaced = false;
    next.blocks.retain_mut(|block| {
        if block.section != section || block.body.authored().is_none() {
            return true;
        }
        if replaced {
            return false;
        }
        block.body = BlockBody::Text {
            text: body.to_string(),
        };
        replaced = true;
        true
    });
    if !replaced {
        return Err(Rejection::NoTextBlock { section });
    }
    if !next
        .blocks
        .iter()
        .any(|b| b.section == section && !b.optional)
    {
        return Err(Rejection::WouldEmitNothing { section });
    }
    next.validate()
        .map_err(|error| Rejection::Invalidates { section, error })?;
    Ok(next)
}

impl InstructionPackage {
    /// Apply project-tier named-section overrides, reporting every decline.
    ///
    /// Why: this is the single decision point for "may this override land?", and
    /// it asks the package's own `customization_tier` rather than any list held
    /// by the reader. A section is overridable if and only if the shipped
    /// package says so, which is what makes the floor structurally unreachable
    /// from `CLAUDE.md` rather than merely unlisted.
    ///
    /// What: applies overrides in [`SectionId`] canonical order (so the result is
    /// independent of authoring order), validating after each one. An override
    /// that fails any rule — floor tier, no text block, empty body, a section
    /// left unable to emit, or a package that stops validating — is discarded on
    /// its own and reported; the others still land. A final validation guards the
    /// whole, and on failure the pristine package is returned instead, so the
    /// worst case is the bundled prompt rather than a broken one.
    ///
    /// An empty `overrides` slice returns a clone, so the composed bytes of a
    /// project with no `CLAUDE.md` cannot move.
    ///
    /// Test: `with_overrides_of_nothing_is_the_identity`,
    /// `floor_sections_refuse_every_named_section_override`,
    /// `one_bad_override_does_not_discard_a_good_one`.
    pub fn with_overrides(&self, overrides: &[SectionOverride]) -> (Self, Vec<Rejection>) {
        let mut ordered: Vec<&SectionOverride> = overrides.iter().collect();
        ordered.sort_by_key(|o| o.section);

        let mut working = self.clone();
        let mut rejected = Vec::new();
        for over in ordered {
            match apply_one(&working, over) {
                Ok(next) => working = next,
                Err(rejection) => rejected.push(rejection),
            }
        }

        // Unreachable while `apply_one` validates each step — kept because the
        // cost of being wrong here is a malformed system prompt, and the
        // degradation (compose the bundled package) is free.
        if let Err(error) = working.validate() {
            rejected.push(Rejection::PackageInvalid(error));
            return (self.clone(), rejected);
        }
        (working, rejected)
    }
}

#[cfg(test)]
#[path = "claude_md_sections_tests.rs"]
mod tests;
