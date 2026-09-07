//! A bounded, scrubbed excerpt of the code each RED finding cites (#6792).
//!
//! Why: the recipient of a return package holds no checkout. Every row in a
//! repository's `[report].findings` names a file they cannot open, so a RED
//! finding arrives as a claim with no way to check it — the recipient either
//! takes the collector's word or asks the client to go and look. Carrying the
//! lines around the cited location closes that, and it is the one thing in the
//! package that is verbatim client source, which is why it is off by default:
//! `[excerpts] enabled` in the engagement config turns it on, and an engagement
//! that bars code excerpts leaves it alone.
//!
//! What: [`render`] reads each audited repository's manifest through
//! [`crate::debt_rollup::read_finding_rows`] — the same parse the debt roll-up
//! uses — keeps the rows whose band is [`crate::debt_rollup::RED_TIER`], and
//! writes one entry per row into [`super::EXCERPTS_ENTRY`]. An entry carries
//! either the excerpt or a `unavailable` sentence saying why there is none.
//! There is no third state: a RED finding is never silently absent from this
//! member, because an absence is what a reader cannot tell from a collector
//! that did not run.
//!
//! ## What bounds the excerpt
//!
//! Three bounds, all of them enforced here rather than trusted of the input:
//!
//! - **Lines.** At most `2 × context_lines + 1`, from
//!   [`crate::config::ExcerptSettings::context_lines`], which clamps whatever
//!   the config declared.
//! - **Line length.** A line longer than [`MAX_LINE_CHARS`] is truncated at a
//!   char boundary and the entry records `truncated`. A minified bundle is one
//!   line of two megabytes, and "five lines either side" is no bound at all
//!   over a file shaped like that.
//! - **Location.** [`resolve_in_checkout`] refuses an absolute path, refuses
//!   any `..` component, and — after `canonicalize` has followed every
//!   symlink — refuses a file that does not sit under the checkout the finding
//!   belongs to. A finding cites a path a third-party tool wrote into a
//!   manifest; nothing about it is trusted.
//!
//! ## Scrubbed here, refused in `fill_archive`
//!
//! The same two guards, in the same order, that [`super::error_digest`] applies
//! and for the same reason — see that module's docs. Every excerpt goes through
//! [`trusty_common::credentials::scrub_secrets`] over
//! [`crate::config::EngagementConfig::configured_secrets`] plus the `gh`-derived
//! token as it is built, and
//! [`super::credential_scan::refuse_if_credential`] then scans the assembled
//! document in [`super::fill_archive`]. Neither is redundant: `scrub_secrets`
//! declines a needle shorter than its minimum, so the refusal is what still
//! catches a short secret.
//!
//! ## The `secrets` category is never excerpted
//!
//! `crate::grounding::secrets` redacts the matched value before it constructs a
//! row, so no secret gitleaks found reaches the manifest. Excerpting the lines
//! AROUND that match would put it straight back — and in the file shape that
//! collector fires on most (a `.env`, a fixture of keys) the neighbours are
//! credentials too, so there is no window that is safe to quote. This member
//! declines that category outright rather than reaching for a second redactor
//! over content nothing has classified. The entry still exists and states the
//! reason, so the recipient sees the ruling rather than an absence.
//!
//! Test: `super::package_tests`.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::EngagementConfig;
use crate::debt_rollup::{self, FindingRow};
use crate::error::AuditError;
use crate::run::{RepoRun, SelectedRepo};
use crate::workdir::WorkDir;

/// Characters one excerpt line may carry before it is truncated.
///
/// Why: a generated or minified file is a handful of enormous lines, and the
/// line budget alone would let one finding carry megabytes into a package that
/// is otherwise reports and counts. 400 is wide enough that ordinary source —
/// including a long Rust signature or a deeply indented call — arrives whole.
const MAX_LINE_CHARS: usize = 400;

/// Appended to a line [`MAX_LINE_CHARS`] cut short, so the cut is visible.
const TRUNCATION_MARKER: &str = "…";

/// The `evidence/excerpts.json` document.
#[derive(Debug, Serialize)]
struct ExcerptDocument {
    generated_by: String,
    /// When the member was written — the package's clock, not the sweep's.
    generated_at: String,
    /// Lines either side of a cited line, after the config's clamp.
    context_lines: u64,
    /// One entry per RED finding, in the order the manifests declare them.
    /// Empty when no repository declared one, never absent.
    entries: Vec<ExcerptEntry>,
}

/// One RED finding, and either its excerpt or why it has none.
#[derive(Debug, Serialize)]
struct ExcerptEntry {
    /// The repository whose manifest declared the finding.
    repository: String,
    /// The collector that produced it — the finding's `category`.
    category: String,
    /// The advisory, rule or licence identifier — the finding's `id`.
    id: String,
    /// The band, as [`debt_rollup::normalised_tier`] spells it. Always `RED`.
    severity: String,
    /// What the row cited, verbatim, before this module interpreted it.
    cites: String,
    /// The repo-relative file the excerpt came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    /// The cited line, 1-based. `None` when the row named a file and no line,
    /// in which case the window is the head of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    cited_line: Option<u64>,
    /// First line of the window, 1-based and inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    first_line: Option<u64>,
    /// Last line of the window, 1-based and inclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_line: Option<u64>,
    /// Whether any line in the excerpt was cut at [`MAX_LINE_CHARS`].
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    /// The lines themselves, scrubbed. Absent exactly when `unavailable` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt: Option<String>,
    /// Why there is no excerpt. Absent exactly when `excerpt` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable: Option<String>,
}

impl ExcerptEntry {
    /// The stated-reason form: everything the row said, and no lines.
    fn unavailable(row: &Row<'_>, reason: String) -> Self {
        Self {
            repository: row.repository.to_owned(),
            category: row.finding.category.clone(),
            id: row.finding.id.clone(),
            severity: debt_rollup::RED_TIER.to_owned(),
            cites: row.cites.clone(),
            file: None,
            cited_line: None,
            first_line: None,
            last_line: None,
            truncated: false,
            excerpt: None,
            unavailable: Some(reason),
        }
    }
}

/// One RED finding being considered, with the repository it belongs to.
struct Row<'a> {
    repository: &'a str,
    checkout: PathBuf,
    finding: FindingRow,
    /// The row's own words for the location — `file:line`, `file`, or a crate
    /// name — carried through to the entry so a reader can see what was read.
    cites: String,
}

/// Build `evidence/excerpts.json`, or `None` when the engagement bars excerpts.
///
/// # Preconditions
/// `audited` holds the repositories whose reports are going into the archive;
/// a repository that failed has no manifest to read and belongs elsewhere.
///
/// # Postconditions
/// On `Ok(Some(text))` the text is a JSON object whose `entries` array carries
/// exactly one element per RED finding across those repositories — each with an
/// `excerpt` or an `unavailable`, never both and never neither — in which no
/// configured secret and no `gh`-derived token appears in a scrubbable form, no
/// excerpt exceeds `2 × context_lines + 1` lines, and no excerpt line comes
/// from outside the citing repository's own checkout. `Ok(None)` exactly when
/// [`crate::config::ExcerptSettings::enabled`] is false.
///
/// What: reads each repository's manifest once, keeps the RED rows, and asks
/// [`entry_for`] for each. `github_token` is the same extra needle
/// [`super::error_digest::render`] takes, and reaches this process from the
/// recipient's `gh` keychain rather than from the engagement config.
/// Test: `super::package_tests::{a_red_finding_at_a_file_and_line_carries_an_excerpt,
/// a_non_red_finding_carries_no_excerpt,
/// an_excerpt_carrying_a_configured_secret_is_scrubbed_not_shipped,
/// a_finding_whose_file_cannot_be_read_states_why,
/// an_excerpt_never_reaches_outside_the_checkout,
/// the_excerpt_member_is_absent_unless_the_engagement_asks_for_it}`.
///
/// # Errors
///
/// [`AuditError::Package`] when the document cannot be serialised, which needs
/// a `serde_json` failure over plain owned strings.
pub(super) fn render(
    work: &WorkDir,
    config: &EngagementConfig,
    audited: &[&RepoRun],
    github_token: Option<&str>,
) -> Result<Option<String>, AuditError> {
    if !config.excerpts.enabled {
        return Ok(None);
    }
    let context_lines = config.excerpts.context_lines();
    let mut needles: Vec<String> = config
        .configured_secrets()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Some(token) = github_token {
        needles.push(token.to_owned());
    }

    let mut entries = Vec::new();
    for run in audited {
        let manifest = run.output.join(crate::manifest::AuditManifest::FILE_NAME);
        let checkout = checkout_root(work, &run.repo);
        for finding in debt_rollup::read_finding_rows(&manifest) {
            if debt_rollup::normalised_tier(&finding.severity) != debt_rollup::RED_TIER {
                continue;
            }
            let cites = cited_text(&finding);
            let row = Row {
                repository: &run.repo.name,
                checkout: checkout.clone(),
                finding,
                cites,
            };
            entries.push(entry_for(&row, context_lines, &needles));
        }
    }

    let document = ExcerptDocument {
        generated_by: format!("trusty-audit {}", env!("CARGO_PKG_VERSION")),
        generated_at: crate::index_report::local_now(),
        context_lines,
        entries,
    };
    serde_json::to_string_pretty(&document)
        .map(Some)
        .map_err(|e| AuditError::Package {
            path: PathBuf::from(super::EXCERPTS_ENTRY),
            source: std::io::Error::other(e),
        })
}

/// A selection path, anchored to the work-dir root when it is relative.
///
/// // #6792: the same rule `crate::run`'s private `absolute_checkout` applies.
/// It stays private there because this change does not touch `run.rs`; the two
/// collapse into one when it is next edited.
fn checkout_root(work: &WorkDir, repo: &SelectedRepo) -> PathBuf {
    if repo.path.is_absolute() {
        repo.path.clone()
    } else {
        work.root().join(&repo.path)
    }
}

/// What a row says its finding is about, in the row's own words.
///
/// The `file`/`line` columns win when a collector states them, because they are
/// unambiguous; `package` is what today's four collectors use and carries a
/// crate name as readily as a path.
fn cited_text(finding: &FindingRow) -> String {
    match (finding.file.as_deref(), finding.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.to_owned(),
        (None, _) => finding.package.clone(),
    }
}

/// The `(path, line)` a row cites, or `None` when it cites neither.
///
/// `package` is parsed as `path:line` — the shape `crate::grounding::secrets`
/// writes — only when the segment after the last colon is entirely digits. A
/// crate name has no colon and a Windows-style `C:\…` fails the digit test, so
/// neither is mistaken for a location.
fn cited_location(finding: &FindingRow) -> Option<(String, Option<u64>)> {
    if let Some(file) = finding
        .file
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        return Some((file.to_owned(), finding.line));
    }
    let package = finding.package.trim();
    if package.is_empty() {
        return None;
    }
    match package.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() && !line.is_empty() => match line.parse::<u64>() {
            Ok(line) => Some((path.to_owned(), Some(line))),
            Err(_) => Some((package.to_owned(), None)),
        },
        _ => Some((package.to_owned(), None)),
    }
}

/// One RED finding's entry: its excerpt, or the sentence saying why not.
fn entry_for(row: &Row<'_>, context_lines: u64, needles: &[String]) -> ExcerptEntry {
    if row.finding.category.trim() == crate::grounding::secrets::CATEGORY {
        return ExcerptEntry::unavailable(
            row,
            "the secrets category is never excerpted — the matched value is redacted before it \
             reaches the manifest, and the lines around it are as likely to be credentials"
                .to_owned(),
        );
    }
    let Some((cited, line)) = cited_location(&row.finding) else {
        return ExcerptEntry::unavailable(row, "the finding cites no location".to_owned());
    };
    let path = match resolve_in_checkout(&row.checkout, &cited) {
        Ok(path) => path,
        Err(reason) => return ExcerptEntry::unavailable(row, reason),
    };
    match read_window(&path, line, context_lines) {
        Ok(window) => ExcerptEntry {
            repository: row.repository.to_owned(),
            category: row.finding.category.clone(),
            id: row.finding.id.clone(),
            severity: debt_rollup::RED_TIER.to_owned(),
            cites: row.cites.clone(),
            file: Some(cited),
            cited_line: line,
            first_line: Some(window.first_line),
            last_line: Some(window.last_line),
            truncated: window.truncated,
            excerpt: Some(trusty_common::credentials::scrub_secrets(
                &window.text,
                needles,
            )),
            unavailable: None,
        },
        Err(reason) => ExcerptEntry::unavailable(row, reason),
    }
}

/// A cited path resolved to a real file inside `checkout`, or why it is not one.
///
/// Why: the cited path reaches this process from a manifest a third-party
/// collector wrote. Treating it as a location to open is the whole risk this
/// member carries, so every way out of the checkout is refused explicitly
/// rather than assumed away — an absolute path, a `..` component, and a symlink
/// pointing out of the tree, which the `canonicalize` comparison catches after
/// the component check cannot.
///
/// # Postconditions
/// On `Ok`, the returned path is an existing regular file whose canonical form
/// is inside `checkout`'s canonical form.
///
/// Test: `super::package_tests::{an_excerpt_never_reaches_outside_the_checkout,
/// a_finding_whose_file_cannot_be_read_states_why}`.
fn resolve_in_checkout(checkout: &Path, cited: &str) -> Result<PathBuf, String> {
    let relative = Path::new(cited);
    if relative.is_absolute() {
        return Err(format!(
            "`{cited}` is an absolute path — only a repo-relative one is excerpted"
        ));
    }
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "`{cited}` leaves the checkout — only a plain repo-relative path is excerpted"
        ));
    }
    let root = checkout
        .canonicalize()
        .map_err(|e| format!("the checkout for this repository could not be resolved ({e})"))?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| format!("`{cited}` could not be resolved in the checkout ({e})"))?;
    if !path.starts_with(&root) {
        return Err(format!("`{cited}` resolves outside the checkout"));
    }
    if !path.is_file() {
        return Err(format!("`{cited}` is not a regular file"));
    }
    Ok(path)
}

/// The lines a window covers, and whether any of them was cut.
struct Window {
    text: String,
    first_line: u64,
    last_line: u64,
    truncated: bool,
}

/// Read at most `2 × context` + 1 lines around `line`, or the file's head.
///
/// Why: read line by line rather than into a `String`. The bound is the point
/// of this member, and reading the whole file first would give a generated
/// 200 MB blob a way past it before any budget applied.
/// What: a 1-based, inclusive window. `line` of `None` — a finding that cites a
/// file and no line — takes the head of the file, which is what "around the
/// cited line" reduces to when there is no cited line. A `line` past the end of
/// the file yields as much of the tail as the window covers, and a file with
/// nothing in the window at all is an error rather than an empty excerpt.
///
/// # Errors
/// One sentence, safe to show the recipient, when the file cannot be opened or
/// read — which includes a file that is not valid UTF-8.
///
/// Test: `super::package_tests::{a_red_finding_at_a_file_and_line_carries_an_excerpt,
/// an_excerpt_line_longer_than_the_cap_is_cut}`.
fn read_window(path: &Path, line: Option<u64>, context: u64) -> Result<Window, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("could not be read ({e})"))?;
    let (first, last) = match line {
        Some(cited) => (cited.saturating_sub(context).max(1), cited + context),
        None => (1, context * 2 + 1),
    };

    let mut kept: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut first_kept = 0_u64;
    let mut last_kept = 0_u64;
    let mut number = 0_u64;
    for read in std::io::BufReader::new(file).lines() {
        number += 1;
        if number > last {
            break;
        }
        let text = read.map_err(|e| format!("could not be read ({e})"))?;
        if number < first {
            continue;
        }
        if first_kept == 0 {
            first_kept = number;
        }
        last_kept = number;
        let (text, cut) = clamp_line(&text);
        truncated |= cut;
        kept.push(text);
    }

    if kept.is_empty() {
        return Err(format!(
            "the file has no line {}",
            line.map_or_else(|| "to excerpt".to_owned(), |l| l.to_string())
        ));
    }
    Ok(Window {
        text: kept.join("\n"),
        first_line: first_kept,
        last_line: last_kept,
        truncated,
    })
}

/// One line, cut at [`MAX_LINE_CHARS`] on a char boundary.
///
/// Counts CHARACTERS, not bytes, and cuts on a `char_indices` boundary, so a
/// line of multi-byte text is bounded without ever slicing a code point in two.
fn clamp_line(text: &str) -> (String, bool) {
    match text.char_indices().nth(MAX_LINE_CHARS) {
        Some((at, _)) => (format!("{}{TRUNCATION_MARKER}", &text[..at]), true),
        None => (text.to_owned(), false),
    }
}
