//! The dependency CVE scan the DD report used to disclaim (#6075, epic #6074).
//!
//! Why: the report's Security Posture section states, correctly, that its
//! findings are an LLM's reading of selected source files and NOT a dependency
//! CVE scan. Nothing in the pipeline then performed one, so "CVE exposure" was
//! named as an assurance gap and left there. `cargo audit` answers the question
//! deterministically for a Rust target in one subprocess, and the answer belongs
//! in the report rather than in this process's stderr.
//!
//! Owner ruling 2026-08-19 puts the collector HERE rather than in trusty-review:
//! all collector intelligence lives in trusty-audit and the manifest is the
//! interface, so tuning what is scanned is a single-crate rebuild.
//!
//! What: one leg, `cargo-audit audit --json` against the checkout, reduced to
//! one [`Advisory`] per row. [`write_into`] puts them in `[report].findings`,
//! where trusty-review renders them; [`ground_into`] is the caller-facing shape
//! every other grounding leg has — gap lines and nothing else to decide about.
//!
//! ## Why the subprocess, and why `cargo-audit` rather than `cargo audit`
//!
//! `rustsec` is not in this workspace's lock file and pulling it in would pin a
//! second crate to the advisory-database format for the six fields below.
//! [`super::topology`] next door already spawns its tool exactly this way. The
//! binary is resolved through `trusty_common::bin_resolve::resolve_binary` —
//! the workspace's one answer to "is this tool installed, and where" — rather
//! than through `cargo audit`, because a missing subcommand surfaces from cargo
//! as an exit status among others while an unresolved binary is a fact this
//! module can state precisely and name the install command for.
//!
//! ## Degradation
//!
//! Three outcomes ([`Outcome`]), and the fail-open rule is the one every leg in
//! this module follows: the sweep continues, and the cost is a NAMED line in
//! `[report].gaps`, never a silent zero-findings result (#5620 — a recorded skip
//! permits, a blind gate does not).
//!
//! | Target | Outcome |
//! |---|---|
//! | `Cargo.lock` present | scanned, or a gap naming why the scan failed |
//! | `Cargo.toml` but no lock, or a non-Rust ecosystem | a gap naming the language |
//! | no recognised dependency manifest at all | a declared skip: nothing written, nothing said |
//!
//! The last row is the same distinction [`super::topology`] draws. A repository
//! declaring no dependencies in any ecosystem this module knows has no CVE
//! surface to report on, and the Dependency Inventory section already carries
//! trusty-review's own "not examined / declares nothing" caveat for it — a
//! second line restating it would be noise in every such report.
//!
//! Test: `cve_tests`.

use std::path::Path;
use std::process::Command;

use toml_edit::{InlineTable, Value};

use super::findings::first_line;

/// The collector's name, as it appears at the head of every gap line it writes.
pub const COLLECTOR: &str = "cve-scan";

/// The `category` every advisory this collector records carries.
///
/// The reuse point for the rest of epic #6074: #6076's license collector and
/// #6077's secrets collector write the same `[report].findings` rows under
/// `license` and `secrets`, and trusty-review groups the rendered table by this
/// key. Spelled here because this collector is the one that establishes it.
pub const CATEGORY: &str = "dependencies";

/// The binary that performs the scan.
pub const BINARY: &str = "cargo-audit";

/// What an operator runs to get [`BINARY`], named in the missing-binary gap.
pub const INSTALL_COMMAND: &str = "cargo install cargo-audit";

/// One advisory `cargo audit` reported against the target's lock file.
///
/// Why: these six fields are what a due-diligence reader acts on — which
/// advisory, against which pinned version, how bad, and where to read it. The
/// description cargo-audit also emits is deliberately not carried: it is
/// paragraphs of prose per row, and the URL is the better place for it.
/// What: `severity` is this collector's own band, not a CVSS score — see
/// [`Severity`]. `url` is the advisory's own reference when it states one,
/// else the RUSTSEC page derived from the id.
/// Test: `cve_tests::{a_vulnerability_becomes_a_red_advisory,
/// a_warning_becomes_an_amber_advisory}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Advisory {
    /// Advisory id, e.g. `RUSTSEC-2024-0421`.
    pub id: String,
    /// The affected package's name.
    pub package: String,
    /// The version the lock file pins.
    pub version: String,
    /// The band this row renders under.
    pub severity: Severity,
    /// The advisory's one-line title.
    pub title: String,
    /// Where to read it, when there is somewhere to read it.
    pub url: Option<String>,
}

/// The band an advisory renders under.
///
/// Why: cargo-audit states a CVSS *vector* and not a score, and scoring a
/// vector here would be a second implementation of a published algorithm for a
/// number the report renders as a band anyway. The split cargo-audit itself
/// draws is the one that carries the information: a `vulnerabilities` row is an
/// exploitable defect against a pinned version, a `warnings` row is a
/// maintenance signal (unmaintained, unsound, yanked). That maps onto the
/// report's existing RED/AMBER vocabulary exactly.
/// What: `Red` for every `vulnerabilities.list` row, `Amber` for every
/// `warnings` row. There is no green band — a clean scan produces no rows.
///
/// // #6076: this is the whole band vocabulary trusty-review's `band_rank`
/// reads, so the license leg reuses this type rather than declaring a second
/// enum whose `as_str` returns the same two strings. The variant names below
/// describe the CVE reading of each band; a collector that reuses them owes its
/// own definition of what earns each one.
///
/// Test: `cve_tests::a_warning_becomes_an_amber_advisory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// An exploitable advisory against the pinned version.
    Red,
    /// A maintenance signal: unmaintained, unsound, or yanked.
    Amber,
}

impl Severity {
    /// The band as trusty-review's report vocabulary spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Red => "RED",
            Severity::Amber => "AMBER",
        }
    }
}

/// What the leg produced for one repository.
///
/// Why: "no advisories" has three meanings that must not share a variant, and
/// only one of them is a clean result. See the module docs' table.
/// What: the scan's rows (possibly empty, which IS a clean bill for the pinned
/// dependency set), a declared skip carrying why the leg does not apply, or a
/// failure carrying the one line the caller turns into a gap.
/// Test: `cve_tests::{a_repository_with_no_manifest_is_a_declared_skip,
/// a_non_rust_repository_names_its_language}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No recognised dependency manifest; the reason it does not apply.
    NotApplicable(String),
    /// The scan ran; these are its rows, in the order cargo-audit reported them.
    Scanned(Vec<Advisory>),
    /// The scan could not run, or could not be read; why not.
    Unavailable(String),
}

/// One completed `cargo-audit` invocation, as this module needs to see it.
///
/// Why: `cargo audit` exits NON-ZERO when it finds vulnerabilities, so the exit
/// status alone cannot say whether the run failed — the stdout document does.
/// Carrying both is what lets [`scan_with`] apply that rule in one place, and
/// what lets the failure arms be tested without a `cargo-audit` on the machine.
/// What: whether the process exited 0, plus both streams as text.
/// Test: `cve_tests::a_nonzero_exit_with_readable_json_is_still_a_scan`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Run {
    /// Whether the process exited zero.
    pub success: bool,
    /// The JSON document `--json` writes to stdout.
    pub stdout: String,
    /// Diagnostics, used only to explain a failure.
    pub stderr: String,
}

/// Scan `checkout`, or say why there is no scan.
///
/// Why/What: see the module docs. Runs no subprocess at all unless the checkout
/// has a `Cargo.lock`, so every other repository in a sweep costs one
/// `Path::is_file` per ecosystem marker.
///
/// # Postconditions
/// Never panics and never returns an error: every failure is an
/// [`Outcome::Unavailable`] reason string, safe to show the recipient.
///
/// Test: `cve_tests`.
#[must_use]
pub fn scan(checkout: &Path) -> Outcome {
    scan_with(checkout, run_cargo_audit)
}

/// [`scan`] with the subprocess supplied by the caller.
///
/// Why: the two failure arms this collector MUST get right — the binary is not
/// installed, and the run exited non-zero without usable JSON — are precisely
/// the arms a test cannot reach through a real `cargo-audit`, which is either
/// installed on the machine or not. `run` is the seam, and it also keeps the
/// subprocess out of every unit test, so nothing here spawns a process or
/// touches the network.
/// What: the applicability ladder, then `run`, then [`parse`]. A `run` that
/// hands back an `Err` is [`Outcome::Unavailable`] carrying its reason
/// verbatim. A run whose stdout parses is a scan WHATEVER its exit status —
/// `cargo audit` exits 1 on finding vulnerabilities, which is a successful scan
/// and its most important result. A run whose stdout does not parse is
/// unavailable, and names the exit status so a caller can tell a crash from a
/// format change.
/// Test: `cve_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_nonzero_exit_with_malformed_json_is_a_named_gap,
/// a_nonzero_exit_with_readable_json_is_still_a_scan}`.
#[must_use]
pub fn scan_with<F>(checkout: &Path, run: F) -> Outcome
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    if !checkout.join("Cargo.lock").is_file() {
        return match not_scannable(checkout) {
            Some(reason) => Outcome::Unavailable(reason),
            None => Outcome::NotApplicable(
                "the repository root declares no dependency manifest this collector recognises"
                    .to_string(),
            ),
        };
    }
    let output = match run(checkout) {
        Ok(output) => output,
        Err(cause) => return Outcome::Unavailable(cause),
    };
    match parse(&output.stdout) {
        Ok(advisories) => Outcome::Scanned(advisories),
        Err(cause) if output.success => Outcome::Unavailable(cause),
        Err(cause) => Outcome::Unavailable(format!(
            "{cause}, and `{BINARY}` exited non-zero ({})",
            first_line(&output.stderr)
        )),
    }
}

/// Why this checkout cannot be scanned, when it has a dependency surface anyway.
///
/// Why: a Rust repository with no lock file and a Go repository are both
/// unscannable and owe the report DIFFERENT sentences, and neither may be a
/// silent zero — the acquirer is reading this section for CVE exposure that is
/// there in both cases. `None` is reserved for the declared skip: no manifest
/// this collector recognises, so no exposure it can claim to be missing.
/// What: an unlocked Cargo workspace first, since a Rust repository is the one
/// case where the tool exists and only the input is absent; then the language
/// [`super::ecosystem::detect`] names, spelled with the exact wording #6075
/// asks for. // #6076: the marker table moved to `super::ecosystem` when the
/// license leg needed the same detection with different wording.
/// Test: `cve_tests::{a_non_rust_repository_names_its_language,
/// a_rust_repository_with_no_lockfile_says_so}`.
fn not_scannable(checkout: &Path) -> Option<String> {
    if checkout.join("Cargo.toml").is_file() {
        return Some(format!(
            "{COLLECTOR}: the repository declares a Cargo.toml but no Cargo.lock, so `{BINARY}` \
             has no pinned dependency set to scan"
        ));
    }
    super::ecosystem::detect(checkout)
        .map(|language| format!("{COLLECTOR}: no cargo-audit-equivalent for {language}"))
}

/// Reduce one `cargo audit --json` document to its advisories.
///
/// Why: split from [`scan_with`] so every row shape — a vulnerability, each
/// warning kind, a yanked crate with no advisory record — is testable against a
/// captured document with no `cargo-audit`, no checkout and no subprocess in
/// the test.
/// What: `vulnerabilities.list` rows become [`Severity::Red`]; every array
/// under `warnings`, whatever its key, becomes [`Severity::Amber`] — the key
/// set grows with the advisory database, so it is read rather than enumerated.
/// A row with no resolvable package name is skipped: it has nothing a reader
/// could act on.
///
/// # Errors
/// One line when the document is not JSON, or carries no `vulnerabilities`
/// object — the shape every `--json` run emits, so its absence means the output
/// is not cargo-audit's.
///
/// Test: `cve_tests::{the_fixture_yields_every_row,
/// output_that_is_not_json_is_a_reason}`.
pub fn parse(json: &str) -> Result<Vec<Advisory>, String> {
    let doc: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("`{BINARY}` output is not readable as JSON ({e})"))?;
    let vulnerabilities = doc
        .get("vulnerabilities")
        .ok_or_else(|| format!("`{BINARY}` output declares no `vulnerabilities` object"))?;

    let mut advisories = Vec::new();
    for row in vulnerabilities
        .get("list")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        advisories.extend(advisory(row, Severity::Red));
    }
    if let Some(kinds) = doc.get("warnings").and_then(serde_json::Value::as_object) {
        for rows in kinds.values() {
            for row in rows.as_array().map(Vec::as_slice).unwrap_or_default() {
                advisories.extend(advisory(row, Severity::Amber));
            }
        }
    }
    Ok(advisories)
}

/// One cargo-audit row as an [`Advisory`], when it names a package.
///
/// A yanked-crate warning carries no `advisory` record at all — cargo-audit
/// knows only that the registry withdrew the version — so its `kind` stands in
/// for the id and its title is written here rather than quoted from a record
/// that does not exist.
fn advisory(row: &serde_json::Value, severity: Severity) -> Option<Advisory> {
    let string = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let package = row.get("package");
    let record = row.get("advisory").filter(|a| !a.is_null());
    let kind = string(row, "kind").unwrap_or_else(|| "yanked".to_string());

    let name = package
        .and_then(|p| string(p, "name"))
        .or_else(|| record.and_then(|a| string(a, "package")))?;
    let version = package
        .and_then(|p| string(p, "version"))
        .unwrap_or_else(|| "unknown".to_string());
    let id = record
        .and_then(|a| string(a, "id"))
        .unwrap_or_else(|| kind.to_uppercase());
    let title = record.and_then(|a| string(a, "title")).unwrap_or_else(|| {
        format!("the pinned version is {kind}; no advisory record accompanies it")
    });
    let url = record
        .and_then(|a| string(a, "url"))
        .filter(|u| !u.trim().is_empty())
        .or_else(|| {
            id.starts_with("RUSTSEC-")
                .then(|| format!("https://rustsec.org/advisories/{id}.html"))
        });

    Some(Advisory {
        id,
        package: name,
        version,
        severity,
        title,
        url,
    })
}

/// Run `cargo-audit audit --json` in `checkout`.
///
/// `--json` writes the whole document to stdout, so nothing is streamed and the
/// process is reaped by `output()` before this returns. The working directory
/// is the checkout rather than a `--file` flag, so the only flag surface is
/// `--json` and cargo-audit resolves its own default lock file.
fn run_cargo_audit(checkout: &Path) -> Result<Run, String> {
    let binary = trusty_common::bin_resolve::resolve_binary(BINARY).ok_or_else(|| {
        format!(
            "{COLLECTOR}: `{BINARY}` is not installed, so no dependency CVE scan ran (install it \
             with `{INSTALL_COMMAND}`)"
        )
    })?;
    let output = Command::new(&binary)
        .arg("audit")
        .arg("--json")
        .current_dir(checkout)
        .output()
        .map_err(|e| format!("{COLLECTOR}: `{BINARY}` could not be run ({e})"))?;
    Ok(Run {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Record `advisories` in the manifest at `path` as `[report].findings`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A scan this
/// process performs and does not write reaches no renderer — not the sweep's,
/// and not the recipient's own re-render of the delivered package.
///
/// Why nothing is written for an EMPTY scan: the presence of the key is what
/// trusty-review renders the Assurance Scans section from, and an empty array
/// would put an empty table in the report. A clean scan states itself through
/// its `[report].gaps` scope line instead — see [`ground_into`].
/// What: builds one row per advisory and hands them to
/// [`super::findings::append`], the shared writer — which skips a row already
/// declared under the same id/package/version so a resumed sweep does not
/// restate it. // #6077: that plumbing moved to `super::findings` when the
/// secrets leg needed the identical thirty lines a third time.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, every advisory is declared exactly once in `[report].findings` and
/// nothing else in the document changed. An empty `advisories` writes nothing
/// and cannot fail.
///
/// Test: `cve_tests::{the_advisories_land_in_the_manifest,
/// a_resumed_sweep_does_not_duplicate_a_row}`.
pub fn write_into(path: &Path, advisories: &[Advisory]) -> Result<(), String> {
    let rows: Vec<InlineTable> = advisories.iter().map(row).collect();
    super::findings::append(path, &rows, IDENTITY)
}

/// The keys that identify one declared row, for the resumed-sweep skip.
///
/// Identity is the triple, not the id: one advisory can affect two members of
/// the same sweep at two different versions, and both are worth stating.
const IDENTITY: &[&str] = &["id", "package", "version"];

/// One advisory as the inline table trusty-review deserialises.
fn row(advisory: &Advisory) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("category", Value::from(CATEGORY));
    table.insert("id", Value::from(advisory.id.as_str()));
    table.insert("package", Value::from(advisory.package.as_str()));
    table.insert("version", Value::from(advisory.version.as_str()));
    table.insert("severity", Value::from(advisory.severity.as_str()));
    table.insert("title", Value::from(advisory.title.as_str()));
    if let Some(url) = &advisory.url {
        table.insert("url", Value::from(url.as_str()));
    }
    table
}

/// [`scan`], then write what it produced into `manifest`.
///
/// Why: the same shape [`super::ground_manifest`] gives every other leg — the
/// caller gets gap lines and nothing else to decide about.
/// What: the declared skip writes nothing and says nothing; an unavailable scan
/// is one gap naming `display`, the cause, and what the report therefore will
/// not carry; a scan that found rows writes them, and a write failure becomes a
/// gap of its own. A scan that found NOTHING writes no rows and states its own
/// scope, because "cargo-audit found no advisory" and "no cargo-audit ran" must
/// not read the same on the page.
/// Test: `cve_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_nonzero_exit_with_malformed_json_is_a_named_gap,
/// a_clean_scan_states_its_own_scope}`.
pub fn ground_into(manifest: &Path, checkout: &Path, display: &str) -> Vec<String> {
    ground_into_with(manifest, checkout, display, run_cargo_audit)
}

/// [`ground_into`] with the subprocess supplied by the caller — see [`scan_with`].
pub fn ground_into_with<F>(manifest: &Path, checkout: &Path, display: &str, run: F) -> Vec<String>
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    match scan_with(checkout, run) {
        Outcome::NotApplicable(_) => Vec::new(),
        Outcome::Unavailable(cause) => vec![format!(
            "{display}: {cause} — the report states no dependency CVE exposure for it, which must \
             be read as unassessed rather than as a clean dependency set"
        )],
        Outcome::Scanned(advisories) if advisories.is_empty() => vec![format!(
            "{display}: {COLLECTOR}: `{BINARY}` scanned its Cargo.lock and reported no advisory. \
             Vendored code, build-time downloads, and dependencies outside the Rust lock file are \
             not covered by that scan"
        )],
        Outcome::Scanned(advisories) => match write_into(manifest, &advisories) {
            Ok(()) => Vec::new(),
            Err(cause) => vec![format!(
                "{display}: {COLLECTOR}: {cause} — the report states none of the {} advisory/ies \
                 `{BINARY}` reported for it",
                advisories.len()
            )],
        },
    }
}

#[cfg(test)]
#[path = "cve_tests.rs"]
mod cve_tests;
