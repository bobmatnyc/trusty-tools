//! The dependency license review the DD report used to disclaim (#6076, epic #6074).
//!
//! Why: the report's exec summary names license risk as an assurance gap it
//! does not fill, and Security Posture is an LLM's reading of selected source
//! files — it never sees the license of a transitive dependency at all. An
//! acquirer's first question about a Rust codebase is whether anything in the
//! dependency graph imposes a copyleft obligation on the combined work, and
//! that question has a deterministic answer one subprocess away.
//!
//! Owner ruling 2026-08-19 puts the collector HERE rather than in trusty-review,
//! for the reason [`super::cve`] states: collector intelligence lives in
//! trusty-audit, the manifest is the interface, so tuning the policy below is a
//! single-crate rebuild.
//!
//! What: one leg, `cargo-deny list --format json --layout crate` against the
//! checkout, each crate classified by [`classify`] into at most one
//! [`Finding`]. [`write_into`] puts them in `[report].findings` under the
//! `license` category trusty-review already renders as "License / IP
//! Exposure"; [`ground_into`] is the caller-facing shape every other grounding
//! leg has.
//!
//! ## Why `list` rather than `check licenses`
//!
//! #6076 names `cargo deny check licenses` as the preferred invocation. It is
//! the wrong one for a due-diligence target, and measurably so: `check
//! licenses` reports against a `deny.toml` policy, and a third-party target
//! does not carry one. Run against this very workspace with no config it
//! reports `{"licenses":{"errors":0,"helps":914,...}}` — zero findings over 914
//! crates, which is precisely the silent zero epic #6074 exists to remove.
//! Supplying a generated policy instead would pin this crate to cargo-deny's
//! config schema, which changed shape between its v1 and v2 configs.
//!
//! `list` needs no config, changes shape far less, and hands over the raw
//! license set per crate. The POLICY — what an acquirer must plan around —
//! then lives here, in [`STRONG_COPYLEFT`] / [`WEAK_COPYLEFT`] / [`PERMISSIVE`],
//! where it is uniform across every repository in a sweep rather than being
//! whatever risk appetite each target happened to write down. `cargo-license`,
//! #6076's alternative, is a second binary for the same data; cargo-deny is
//! already the tool this leg's sibling ecosystem needs, so requiring one
//! binary rather than two is the smaller ask of the auditor's machine.
//!
//! ## The one thing `list` cannot tell us
//!
//! It flattens an SPDX expression to a SET, so `GPL-3.0 OR MIT` and
//! `GPL-3.0 AND MIT` both arrive as `["GPL-3.0", "MIT"]`. [`classify`]
//! therefore clears a crate as soon as ANY term is permissive: for the `OR`
//! case that is exactly right (the permissive option is available), and for the
//! far rarer `AND` case it under-reports. The clean-scan gap line in
//! [`ground_into_with`] states that limit rather than leaving the reader to
//! assume it away.
//!
//! ## Degradation
//!
//! Three outcomes ([`Outcome`]), and the fail-open rule every leg in this
//! module follows: the sweep continues, and the cost is a NAMED line in
//! `[report].gaps`, never a silent zero-findings result (#5620 — a recorded
//! skip permits, a blind gate does not).
//!
//! | Target | Outcome |
//! |---|---|
//! | `Cargo.lock` present | reviewed, or a gap naming why the review failed |
//! | `Cargo.toml` but no lock, or a non-Rust ecosystem | a gap naming the language |
//! | no recognised dependency manifest at all | a declared skip: nothing written, nothing said |
//!
//! Test: `license_tests`.

use std::path::Path;
use std::process::Command;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use super::cve::{Run, Severity};

/// The collector's name, as it appears at the head of every gap line it writes.
pub const COLLECTOR: &str = "license-review";

/// The `category` every finding this collector records carries.
///
/// trusty-review's `assurance::subsection_title` already maps it to "License /
/// IP Exposure" — #6075 spelled the three categories epic #6074 defines when it
/// built the renderer, so nothing in that crate changes for this leg.
pub const CATEGORY: &str = "license";

/// The binary that performs the review.
pub const BINARY: &str = "cargo-deny";

/// What an operator runs to get [`BINARY`], named in the missing-binary gap.
pub const INSTALL_COMMAND: &str = "cargo install cargo-deny";

/// The id a crate declaring no license at all is reported under.
///
/// Not an SPDX identifier, and deliberately not one: SPDX's `NONE` means "the
/// expression is absent", while this row means the acquirer has no established
/// right to redistribute the crate. It is the worst finding this leg produces.
pub const UNLICENSED: &str = "UNLICENSED";

// ─── Policy ─────────────────────────────────────────────────────────────────

/// Licenses that impose no obligation on the combined work.
///
/// Why: an acquirer plans around obligations, not around licenses. Everything
/// here asks for attribution and nothing more, so a crate offering any one of
/// them is not a finding — see the module docs on why "offering ANY" is the
/// right test given cargo-deny's flattened set.
/// What: SPDX identifiers, matched after [`normalise`] strips version
/// qualifiers and `WITH` exceptions. `Apache-2.0 WITH LLVM-exception` therefore
/// matches through its `Apache-2.0` base.
/// Test: `license_tests::a_permissive_dependency_set_produces_no_finding`.
pub const PERMISSIVE: &[&str] = &[
    "0BSD",
    "APAFML",
    "Apache-1.1",
    "Apache-2.0",
    "BSD-1-Clause",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC-BY-4.0",
    "CC0-1.0",
    "CDLA-Permissive-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT-0",
    "MPL-2.0-no-copyleft-exception",
    "NCSA",
    "OpenSSL",
    "PostgreSQL",
    "Python-2.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "Unlicense",
    "WTFPL",
    "X11",
    "Zlib",
    "bzip2-1.0.6",
    "libpng-2.0",
    "zlib-acknowledgement",
];

/// Licenses whose obligation reaches the whole combined work.
///
/// Why: this is the finding an acquirer's counsel acts on. Linking one of these
/// into a proprietary product obliges publishing that product's source under
/// the same terms, and AGPL/SSPL extend that to network use — so a single such
/// crate deep in a transitive graph can bind the entire codebase.
/// What: [`Severity::Red`], with the obligation stated in the row's title.
/// Test: `license_tests::a_strong_copyleft_dependency_is_a_red_finding`.
pub const STRONG_COPYLEFT: &[&str] = &[
    "AGPL-1.0",
    "AGPL-3.0",
    "CPAL-1.0",
    "EUPL-1.1",
    "EUPL-1.2",
    "GPL-1.0",
    "GPL-2.0",
    "GPL-3.0",
    "OSL-1.0",
    "OSL-2.1",
    "OSL-3.0",
    "Parity-6.0.0",
    "Parity-7.0.0",
    "RPL-1.5",
    "SSPL-1.0",
];

/// Licenses whose obligation stops at the files they cover.
///
/// Why: real, plannable, and not the same risk as [`STRONG_COPYLEFT`] — a
/// proprietary product may link these provided the covered sources stay
/// available and modifications to them are published. It is a condition on the
/// acquirer's release process, not on their source.
/// What: [`Severity::Amber`].
/// Test: `license_tests::a_weak_copyleft_dependency_is_an_amber_finding`.
pub const WEAK_COPYLEFT: &[&str] = &[
    "APSL-2.0",
    "CC-BY-SA-4.0",
    "CDDL-1.0",
    "CDDL-1.1",
    "EPL-1.0",
    "EPL-2.0",
    "LGPL-2.0",
    "LGPL-2.1",
    "LGPL-3.0",
    "MPL-1.0",
    "MPL-1.1",
    "MPL-2.0",
    "Ms-RL",
    "Sleepycat",
];

// ─── Model ──────────────────────────────────────────────────────────────────

/// One crate in the target's dependency graph, as `cargo-deny list` reported it.
///
/// Why: the collector's own shape rather than cargo-deny's, so [`classify`] is
/// testable against a hand-written graph and [`parse`] is the only place the
/// tool's key format is understood.
/// What: `licenses` is the SPDX term SET, flattened by cargo-deny — see the
/// module docs. An empty vector means the crate declared none.
/// Test: `license_tests::the_fixture_yields_every_crate`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Package {
    /// The crate's name.
    pub name: String,
    /// The version the lock file pins.
    pub version: String,
    /// The SPDX terms cargo-deny resolved for it, in the order it listed them.
    pub licenses: Vec<String>,
}

/// One license obligation this collector is reporting against a pinned crate.
///
/// Why: these five fields are what a due-diligence reader acts on — which
/// license, on which pinned dependency, how binding, and what it obliges.
/// What: `license` is the SPDX term that earned the band, or [`UNLICENSED`].
/// `url` is the SPDX page for a recognised identifier and `None` otherwise,
/// which is also how a reader tells a real identifier from a string the crate
/// invented.
/// Test: `license_tests::{a_strong_copyleft_dependency_is_a_red_finding,
/// a_crate_with_no_license_is_the_worst_finding}`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Finding {
    /// The SPDX term that earned the band, or [`UNLICENSED`].
    pub license: String,
    /// The affected crate's name.
    pub package: String,
    /// The version the lock file pins.
    pub version: String,
    /// The band this row renders under.
    pub severity: Severity,
    /// What the license obliges, in one line.
    pub obligation: String,
    /// The SPDX page, when the term is a recognised identifier.
    pub url: Option<String>,
}

/// What the leg produced for one repository.
///
/// Why: "no license risk" has three meanings that must not share a variant, and
/// only one of them is a clean result. See the module docs' table.
/// What: the review's rows (possibly empty, which IS a clean bill for the
/// pinned dependency set), a declared skip carrying why the leg does not apply,
/// or a failure carrying the one line the caller turns into a gap.
/// Test: `license_tests::{a_repository_with_no_manifest_is_a_declared_skip,
/// a_non_rust_repository_names_its_language}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No recognised dependency manifest; the reason it does not apply.
    NotApplicable(String),
    /// The review ran; these are its rows, in the order cargo-deny listed them.
    Reviewed(Vec<Finding>),
    /// The review could not run, or could not be read; why not.
    Unavailable(String),
}

// ─── Scanning ───────────────────────────────────────────────────────────────

/// Review `checkout`, or say why there is no review.
///
/// Why/What: see the module docs. Runs no subprocess at all unless the checkout
/// has a `Cargo.lock`, so every other repository in a sweep costs one
/// `Path::is_file` per ecosystem marker.
///
/// # Postconditions
/// Never panics and never returns an error: every failure is an
/// [`Outcome::Unavailable`] reason string, safe to show the recipient.
///
/// Test: `license_tests`.
#[must_use]
pub fn review(checkout: &Path) -> Outcome {
    review_with(checkout, run_cargo_deny)
}

/// [`review`] with the subprocess supplied by the caller.
///
/// Why: the failure arms this collector MUST get right — the binary is not
/// installed, and the run produced output this module cannot read — are exactly
/// the arms a test cannot reach through a real `cargo-deny`, which is either
/// installed on the machine or not. `run` is the seam, and it keeps the
/// subprocess out of every unit test, so nothing here spawns a process or
/// touches the network.
/// What: the applicability ladder, then `run`, then [`parse`] and
/// [`classify`]. A `run` that hands back an `Err` is [`Outcome::Unavailable`]
/// carrying its reason verbatim. Unlike [`super::cve::scan_with`], a non-zero
/// exit is NOT a successful review: `cargo-deny list` exits zero whenever it
/// produced a listing, so a non-zero exit means there is no listing to read.
/// Its stderr is quoted so a caller can tell a crash from a format change.
/// Test: `license_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_nonzero_exit_is_a_named_gap, unreadable_output_is_a_named_gap}`.
#[must_use]
pub fn review_with<F>(checkout: &Path, run: F) -> Outcome
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    if !checkout.join("Cargo.lock").is_file() {
        return match not_reviewable(checkout) {
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
    if !output.success {
        return Outcome::Unavailable(format!(
            "{COLLECTOR}: `{BINARY}` exited non-zero without a listing ({})",
            first_line(&output.stderr)
        ));
    }
    match parse(&output.stdout) {
        Ok(packages) => Outcome::Reviewed(packages.iter().filter_map(classify).collect()),
        Err(cause) => Outcome::Unavailable(format!("{COLLECTOR}: {cause}")),
    }
}

/// Why this checkout cannot be reviewed, when it has a dependency surface anyway.
///
/// Why: a Rust repository with no lock file and a Go repository are both
/// unreviewable and owe the report DIFFERENT sentences, and neither may be a
/// silent zero — the acquirer is reading this section for license exposure that
/// is there in both cases. `None` is reserved for the declared skip: no
/// manifest this collector recognises, so no exposure it can claim to be
/// missing.
/// What: an unlocked Cargo workspace first, since a Rust repository is the one
/// case where the tool exists and only the input is absent; then the language
/// [`super::ecosystem::detect`] names.
/// Test: `license_tests::{a_non_rust_repository_names_its_language,
/// a_rust_repository_with_no_lockfile_says_so}`.
fn not_reviewable(checkout: &Path) -> Option<String> {
    if checkout.join("Cargo.toml").is_file() {
        return Some(format!(
            "{COLLECTOR}: the repository declares a Cargo.toml but no Cargo.lock, so `{BINARY}` \
             has no pinned dependency set to review"
        ));
    }
    super::ecosystem::detect(checkout)
        .map(|language| format!("{COLLECTOR}: no cargo-deny-equivalent for {language}"))
}

/// Reduce one `cargo-deny list --layout crate` document to its crates.
///
/// Why: split from [`review_with`] so every row shape — a dual license, a
/// single copyleft term, a crate declaring none — is testable against a
/// captured document with no `cargo-deny`, no checkout and no subprocess.
/// What: the document is one object whose KEY is `"<name> <version> <source>"`
/// and whose value carries a `licenses` array. Name and version are the first
/// two whitespace-separated fields; a key with fewer than two is skipped, since
/// nothing in it identifies a crate a reader could act on.
///
/// # Errors
/// One line when the document is not JSON, or is not the object `--layout
/// crate` emits — so its absence means the output is not cargo-deny's.
///
/// Test: `license_tests::{the_fixture_yields_every_crate,
/// output_that_is_not_json_is_a_reason}`.
pub fn parse(json: &str) -> Result<Vec<Package>, String> {
    let doc: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("`{BINARY}` output is not readable as JSON ({e})"))?;
    let rows = doc
        .as_object()
        .ok_or_else(|| format!("`{BINARY}` output is not the crate-layout listing object"))?;

    let mut packages = Vec::with_capacity(rows.len());
    for (key, value) in rows {
        let mut fields = key.split_whitespace();
        let (Some(name), Some(version)) = (fields.next(), fields.next()) else {
            continue;
        };
        let licenses = value
            .get("licenses")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .map(str::to_string)
            .collect();
        packages.push(Package {
            name: name.to_string(),
            version: version.to_string(),
            licenses,
        });
    }
    Ok(packages)
}

/// The one finding `package` earns, if it earns any.
///
/// Why: this IS the policy. Everything above it is transport.
/// What: a crate declaring nothing is [`Severity::Red`] under [`UNLICENSED`] —
/// no declared license is a stronger claim against redistribution than any
/// copyleft term. Otherwise a crate offering ANY [`PERMISSIVE`] term is
/// cleared, for the reason the module docs give. What remains is banded by its
/// worst term: [`STRONG_COPYLEFT`] is [`Severity::Red`], [`WEAK_COPYLEFT`] and
/// anything unrecognised are [`Severity::Amber`] — an unrecognised term is
/// reported rather than dropped, because a license this table has never seen is
/// the case most needing a human, and dropping it would be the silent zero.
///
/// # Postconditions
/// At most one finding per crate, so a 900-crate graph cannot produce a table
/// longer than its own dependency list.
///
/// Test: `license_tests::{a_strong_copyleft_dependency_is_a_red_finding,
/// a_weak_copyleft_dependency_is_an_amber_finding,
/// a_dual_licensed_crate_takes_its_permissive_option,
/// an_unrecognised_term_is_reported_rather_than_dropped,
/// an_unparsed_compound_expression_is_reported_rather_than_cleared,
/// a_crate_with_no_license_is_the_worst_finding}`.
#[must_use]
pub fn classify(package: &Package) -> Option<Finding> {
    let finding = |license: &str, severity, obligation: String, url| Finding {
        license: license.to_string(),
        package: package.name.clone(),
        version: package.version.clone(),
        severity,
        obligation,
        url,
    };
    if package.licenses.is_empty() {
        return Some(finding(
            UNLICENSED,
            Severity::Red,
            "the crate declares no license, so no right to redistribute it is established"
                .to_string(),
            None,
        ));
    }
    if package
        .licenses
        .iter()
        .any(|term| contains(PERMISSIVE, term))
    {
        return None;
    }
    let (term, severity, obligation) = worst(&package.licenses);
    let url = spdx_url(term);
    Some(finding(term, severity, obligation.to_string(), url))
}

/// The binding term of a license set, with its band and its obligation.
///
/// Strong copyleft outranks weak, which outranks a term this policy has never
/// seen — so a crate under `GPL-3.0 AND LGPL-2.1` is reported as the GPL row it
/// is. The set is non-empty and holds no permissive term when this is reached.
fn worst(licenses: &[String]) -> (&str, Severity, &'static str) {
    const STRONG: &str = "copyleft: linking this into a distributed work obliges releasing that \
                          work's source under the same license";
    const WEAK: &str = "file-level copyleft: modifications to the covered sources must be \
                        published, and the sources kept available";
    const UNKNOWN: &str = "this license is not in the collector's policy table; its obligations \
                           must be reviewed by hand";

    if let Some(term) = licenses.iter().find(|t| contains(STRONG_COPYLEFT, t)) {
        return (term, Severity::Red, STRONG);
    }
    if let Some(term) = licenses.iter().find(|t| contains(WEAK_COPYLEFT, t)) {
        return (term, Severity::Amber, WEAK);
    }
    // Non-empty by the caller's contract; the fallback keeps this total anyway.
    let term = licenses.first().map_or("", String::as_str);
    (term, Severity::Amber, UNKNOWN)
}

/// Whether `table` names `term`, comparing normalised identifiers.
fn contains(table: &[&str], term: &str) -> bool {
    let term = normalise(term);
    table.iter().any(|known| normalise(known) == term)
}

/// An SPDX identifier reduced to the form the policy tables spell.
///
/// Why: SPDX writes one license several ways — `GPL-3.0`, `GPL-3.0-only`,
/// `GPL-3.0-or-later`, `GPL-3.0+` are all the GPL, and `Apache-2.0 WITH
/// LLVM-exception` is Apache-2.0 with a patent carve-out that does not change
/// its obligation. Listing every spelling in three tables would be a table that
/// silently misses the next one.
/// What: lowercased, `WITH <exception>` dropped, then the `-only` / `-or-later`
/// / `+` qualifiers stripped from the tail.
/// Test: `license_tests::spdx_qualifiers_do_not_defeat_the_policy`.
fn normalise(term: &str) -> String {
    let base = term
        .split(" WITH ")
        .next()
        .unwrap_or(term)
        .trim()
        .to_ascii_lowercase();
    let base = base.strip_suffix('+').unwrap_or(&base);
    base.strip_suffix("-or-later")
        .or_else(|| base.strip_suffix("-only"))
        .unwrap_or(base)
        .to_string()
}

/// The SPDX page for a term this policy recognises, else `None`.
///
/// A term absent from all three tables gets no link: inventing an SPDX URL for
/// a string a crate made up would send the reader to a 404 that looks
/// authoritative.
fn spdx_url(term: &str) -> Option<String> {
    let known = contains(PERMISSIVE, term)
        || contains(STRONG_COPYLEFT, term)
        || contains(WEAK_COPYLEFT, term);
    let id = term.split(" WITH ").next().unwrap_or(term).trim();
    (known && !id.is_empty()).then(|| format!("https://spdx.org/licenses/{id}.html"))
}

/// Run `cargo-deny list --format json --layout crate` in `checkout`.
///
/// `--format json` writes the whole listing to stdout, so nothing is streamed
/// and the process is reaped by `output()` before this returns. `--layout
/// crate` is what makes the document one row per crate rather than one per
/// license, which is the shape [`parse`] reads. The working directory is the
/// checkout, so cargo-deny resolves the target's own crate graph.
fn run_cargo_deny(checkout: &Path) -> Result<Run, String> {
    let binary = trusty_common::bin_resolve::resolve_binary(BINARY).ok_or_else(|| {
        format!(
            "{COLLECTOR}: `{BINARY}` is not installed, so no dependency license review ran \
             (install it with `{INSTALL_COMMAND}`)"
        )
    })?;
    let output = Command::new(&binary)
        .arg("list")
        .arg("--format")
        .arg("json")
        .arg("--layout")
        .arg("crate")
        .current_dir(checkout)
        .output()
        .map_err(|e| format!("{COLLECTOR}: `{BINARY}` could not be run ({e})"))?;
    Ok(Run {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The first non-empty line of a diagnostic stream, for a one-line gap.
///
/// // #6720: a leading informational line masking the real cause is the bug
/// that issue records against `index::refusal`. cargo-deny writes no update
/// notice, and this stream is quoted only ALONGSIDE a cause this module already
/// determined ("exited non-zero without a listing") rather than standing in for
/// it — so an unhelpful first line degrades the detail, never the diagnosis.
fn first_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no diagnostic")
}

// ─── Writing ────────────────────────────────────────────────────────────────

/// Record `findings` in the manifest at `path` as `[report].findings`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A review this
/// process performs and does not write reaches no renderer — not the sweep's,
/// and not the recipient's own re-render of the delivered package. This is
/// #6075's channel, reused rather than reinvented: the rows land in the same
/// array, and trusty-review groups them by their `license` category with no
/// change to that crate.
///
/// Why nothing is written for an EMPTY review: the presence of the key is what
/// trusty-review renders the Assurance Scans section from, and an empty array
/// would put an empty table in the report. A clean review states itself through
/// its `[report].gaps` scope line instead — see [`ground_into`].
/// What: appends to `[report].findings`, skipping a row already declared under
/// the same license/package/version so a resumed sweep does not restate it.
/// Written format-preserving with `toml_edit`, exactly as
/// [`super::cve::write_into`] writes its advisories, so the two other crates
/// that own this document keep their key order and their comments.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, every finding is declared exactly once in `[report].findings` and
/// nothing else in the document changed. An empty `findings` writes nothing and
/// cannot fail.
///
/// Test: `license_tests::{the_findings_land_in_the_manifest,
/// a_resumed_sweep_does_not_duplicate_a_row,
/// a_cve_row_already_present_is_left_alone}`.
pub fn write_into(path: &Path, findings: &[Finding]) -> Result<(), String> {
    if findings.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", path.display()))?;

    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    let item = table
        .entry("findings")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())));
    let array = item
        .as_array_mut()
        .ok_or_else(|| "the manifest's `report.findings` is not an array".to_string())?;

    for finding in findings {
        if array.iter().any(|declared| same_row(declared, finding)) {
            continue;
        }
        let mut value = Value::InlineTable(row(finding));
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// Whether a declared row is this license against this pinned crate.
///
/// Identity is the triple, not the license: one copyleft term binds many crates
/// in a sweep, and each is worth stating. The `category` is not compared —
/// nothing else writes a `license`-banded row for the same crate and version,
/// and comparing it would let a category rename duplicate every row.
fn same_row(declared: &Value, finding: &Finding) -> bool {
    let Some(table) = declared.as_inline_table() else {
        return false;
    };
    let field = |key: &str| table.get(key).and_then(Value::as_str);
    field("id") == Some(finding.license.as_str())
        && field("package") == Some(finding.package.as_str())
        && field("version") == Some(finding.version.as_str())
}

/// One finding as the inline table trusty-review deserialises.
///
/// The key names are `ManifestFinding`'s, not this module's: `id` carries the
/// license and `title` the obligation, which is what puts the SPDX term in the
/// rendered table's "Finding" column and the obligation in its "Summary".
fn row(finding: &Finding) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("category", Value::from(CATEGORY));
    table.insert("id", Value::from(finding.license.as_str()));
    table.insert("package", Value::from(finding.package.as_str()));
    table.insert("version", Value::from(finding.version.as_str()));
    table.insert("severity", Value::from(finding.severity.as_str()));
    table.insert("title", Value::from(finding.obligation.as_str()));
    if let Some(url) = &finding.url {
        table.insert("url", Value::from(url.as_str()));
    }
    table
}

/// [`review`], then write what it produced into `manifest`.
///
/// Why: the same shape [`super::ground_manifest`] gives every other leg — the
/// caller gets gap lines and nothing else to decide about.
/// What: the declared skip writes nothing and says nothing; an unavailable
/// review is one gap naming `display`, the cause, and what the report therefore
/// will not carry; a review that found rows writes them, and a write failure
/// becomes a gap of its own. A review that found NOTHING writes no rows and
/// states its own scope, because "cargo-deny found no copyleft obligation" and
/// "no license review ran" must not read the same on the page — and because the
/// `AND`-expression limit in the module docs is a caveat on the clean result,
/// not on the failed one.
/// Test: `license_tests::{an_uninstalled_binary_is_a_named_gap,
/// a_nonzero_exit_is_a_named_gap, a_clean_review_states_its_own_scope,
/// a_manifest_that_cannot_be_written_is_a_named_gap}`.
pub fn ground_into(manifest: &Path, checkout: &Path, display: &str) -> Vec<String> {
    ground_into_with(manifest, checkout, display, run_cargo_deny)
}

/// [`ground_into`] with the subprocess supplied by the caller — see [`review_with`].
pub fn ground_into_with<F>(manifest: &Path, checkout: &Path, display: &str, run: F) -> Vec<String>
where
    F: FnOnce(&Path) -> Result<Run, String>,
{
    match review_with(checkout, run) {
        Outcome::NotApplicable(_) => Vec::new(),
        Outcome::Unavailable(cause) => vec![format!(
            "{display}: {cause} — the report states no dependency license exposure for it, which \
             must be read as unassessed rather than as a permissively licensed dependency set"
        )],
        Outcome::Reviewed(findings) if findings.is_empty() => vec![format!(
            "{display}: {COLLECTOR}: `{BINARY}` reviewed its Cargo.lock and every dependency \
             offers a permissive license. Vendored code, build-time downloads, dependencies \
             outside the Rust lock file, and a crate combining a permissive term WITH a copyleft \
             one in a single expression are not covered by that review"
        )],
        Outcome::Reviewed(findings) => match write_into(manifest, &findings) {
            Ok(()) => Vec::new(),
            Err(cause) => vec![format!(
                "{display}: {COLLECTOR}: {cause} — the report states none of the {} license \
                 obligation(s) `{BINARY}` reported for it",
                findings.len()
            )],
        },
    }
}

#[cfg(test)]
#[path = "license_tests.rs"]
mod license_tests;
