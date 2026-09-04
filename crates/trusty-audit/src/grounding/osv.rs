//! Known vulnerabilities for the scanned dependency set, from OSV.dev (#6780).
//!
//! Why: the bundle records each repository's dependencies as
//! `{name, ecosystem, version}` and matches none of them against an advisory
//! database. [`super::cve`] answers the question for a Rust lock file only, in
//! one `cargo-audit` subprocess, so every npm / PyPI / Go / Maven dependency in
//! an engagement reached the report unmatched — and the Dependency Inventory
//! section listed them beside a Security Posture section that disclaimed having
//! checked them. OSV.dev covers all of those ecosystems behind one batch
//! endpoint, so the match can be made deterministically rather than left to a
//! post-hoc external scan.
//!
//! What: one opt-in leg. It reads the dependency inventory `trusty-review`
//! already measured (see [`inventory`]), maps each row's ecosystem onto an OSV
//! ecosystem name, asks `POST /v1/querybatch` in chunks of
//! [`MAX_QUERIES_PER_BATCH`] ([`super::osv_query`]), and writes what came back
//! three ways — `osv.json` beside the manifest, `[report].findings` rows under
//! [`CATEGORY`], and a roll-up the run index renders
//! ([`super::osv_rollup`]).
//!
//! ## Opt-in, and why
//!
//! It is the only leg in this module that tells a third party the recipient's
//! dependency names. That is a disclosure an engagement consents to rather than
//! inherits, so `[collectors] osv` defaults to false and a run that does not set
//! it is byte-identical to one made before this module existed — apart from the
//! run index, which states the leg did not run rather than staying silent.
//!
//! ## Degradation
//!
//! Fail-open, and never silently. Every coordinate that is not queried, and
//! every batch that does not answer, is a NAMED line in `[report].gaps` and an
//! `errors` entry in `osv.json`. A clean scan states its own scope for the
//! reason [`super::cve`] does: "OSV matched nothing" and "no OSV query ran"
//! must not read the same on the page.
//!
//! ## The one thing this leg cannot fix from here
//!
//! Its input is capped. `trusty_review::report::investigate::deps::MAX_ROWS`
//! truncates the inventory at 30 rows before it reaches the snapshot, so a
//! workspace with 134 dependencies offers 30 of them to query. That cap is
//! stated as a gap rather than worked around: re-deriving the inventory here
//! would be a second implementation of a capability another crate owns
//! (CLAUDE.md's common-entry-point rule). Lifting it belongs in the producer.
//!
//! ## When the rows reach a report
//!
//! The same answer every leg in this module gives. `tga audit` writes the
//! manifest and renders from it inside ONE process, and this leg edits that
//! file once the child has exited — so the rows reach a re-render
//! (`taudit render`), not the report the sweep itself produced. `osv.json` and
//! the run-index roll-up are in the bundle either way.
//!
//! Test: `osv_tests`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{InlineTable, Value};

use super::cve;
use super::osv_query::{self, Settings};

/// The collector's name, at the head of every gap line it writes.
pub const COLLECTOR: &str = "osv-scan";

/// The `category` every row this collector records carries.
///
/// Spelled as a HEADING rather than as a slug, unlike the four categories epic
/// #6074 defines. `trusty_review::report::assurance::subsection_title` titles a
/// category it does not know by the category string itself — its documented
/// extension point for "a collector added after this crate was built". That
/// fallback is the only channel available here: an engagement pins a PUBLISHED
/// `trusty-review`, so a renderer change would not reach a run until that pin
/// was bumped, while this row renders under the right heading today.
pub const CATEGORY: &str = "Known vulnerabilities (OSV)";

/// The per-repository result file this leg writes into the bundle.
pub const SCAN_FILE: &str = "osv.json";

/// The snapshot the dependency inventory is read from — `trusty-review`'s.
pub const INVENTORY_FILE: &str = "investigation.json";

/// Queries one `POST /v1/querybatch` call may carry, per OSV's documented cap.
pub const MAX_QUERIES_PER_BATCH: usize = 1000;

/// Where one advisory is read, given its id.
pub const ADVISORY_URL: &str = "https://osv.dev/vulnerability/";

/// How many rows the run index's "top items" table names.
pub const TOP_ITEMS: usize = 10;

/// One package version to ask OSV about.
///
/// Why: OSV keys an answer on the triple and nothing else, so this is both the
/// query and the cache key ([`super::osv_query`]).
/// What: `ecosystem` is already the OSV spelling ([`osv_ecosystem`]), not the
/// inventory's own label.
/// Test: `osv_tests::the_inventory_becomes_osv_coordinates`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Coordinate {
    /// The OSV ecosystem name, e.g. `crates.io`.
    pub ecosystem: String,
    /// The package name as that ecosystem spells it.
    pub name: String,
    /// The exact pinned version. OSV cannot answer a constraint.
    pub version: String,
}

impl Coordinate {
    /// A coordinate from its three parts.
    #[must_use]
    pub fn new(ecosystem: &str, name: &str, version: &str) -> Self {
        Self {
            ecosystem: ecosystem.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    /// How this coordinate is named in a gap line.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}@{} ({})", self.name, self.version, self.ecosystem)
    }
}

/// The band one advisory renders under.
///
/// Why: OSV states a CVSS *vector* and, separately and inconsistently, a
/// qualitative label. Scoring the vector here would be a second implementation
/// of a published algorithm for a number the report renders as a band anyway —
/// the call [`super::cve`] already made for the same reason.
/// What: the qualitative label when OSV states one, else [`Severity::Unknown`].
/// Declaration order is worst-first, so a sort puts `Critical` at the top.
///
/// `Unknown` bands RED rather than AMBER: it is an advisory OSV published
/// without a severity, and reading that as "probably minor" is the quiet
/// failure this module exists to avoid.
/// Test: `osv_tests::osv_severity_labels_map_onto_report_bands`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    /// OSV said `CRITICAL`.
    Critical,
    /// OSV said `HIGH`.
    High,
    /// OSV said `MODERATE` or `MEDIUM`.
    Moderate,
    /// OSV said `LOW`.
    Low,
    /// OSV published the advisory without a severity label.
    #[default]
    Unknown,
}

impl Severity {
    /// Every label, worst first — the order the count table renders in.
    pub const ALL: [Severity; 5] = [
        Severity::Critical,
        Severity::High,
        Severity::Moderate,
        Severity::Low,
        Severity::Unknown,
    ];

    /// The label as this collector reports it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "CRITICAL",
            Severity::High => "HIGH",
            Severity::Moderate => "MODERATE",
            Severity::Low => "LOW",
            Severity::Unknown => "UNKNOWN",
        }
    }

    /// The report band this label renders under — see [`cve::Severity`].
    #[must_use]
    pub fn band(self) -> cve::Severity {
        match self {
            Severity::Moderate | Severity::Low => cve::Severity::Amber,
            _ => cve::Severity::Red,
        }
    }

    /// The label OSV stated, however it spelled it.
    #[must_use]
    pub fn parse(label: &str) -> Self {
        match label.trim().to_ascii_uppercase().as_str() {
            "CRITICAL" => Severity::Critical,
            "HIGH" => Severity::High,
            "MODERATE" | "MEDIUM" => Severity::Moderate,
            "LOW" => Severity::Low,
            _ => Severity::Unknown,
        }
    }
}

/// One advisory OSV returned against a coordinate.
///
/// `summary` is empty whenever `querybatch` answered with the id alone — the
/// endpoint is an index over ids and is permitted to omit everything else. That
/// is recorded as it arrived rather than filled in: hydrating each id through
/// `/v1/vulns/{id}` is one request per advisory and belongs behind its own knob.
/// Test: `osv_tests::an_id_only_answer_is_still_a_vulnerability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Vuln {
    /// The OSV id, e.g. `RUSTSEC-2024-0421` or a `GHSA-…`.
    pub id: String,
    /// Other ids for the same advisory — the CVE ids, when OSV states them.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// OSV's one-line summary, or empty when the answer carried none.
    #[serde(default)]
    pub summary: String,
    /// The band, from OSV's own qualitative label.
    #[serde(default)]
    pub severity: Severity,
}

impl Vuln {
    /// Where a reader goes to read this advisory.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{ADVISORY_URL}{}", self.id)
    }

    /// The one-line title a report row carries.
    ///
    /// OSV's summary when it stated one; otherwise the aliases, which is what a
    /// reader recognises when the id itself is a `GHSA-…`; otherwise a sentence
    /// saying the endpoint returned the id alone, rather than an empty cell.
    #[must_use]
    pub fn title(&self) -> String {
        let summary = self.summary.trim();
        if !summary.is_empty() {
            return summary.to_owned();
        }
        if !self.aliases.is_empty() {
            return format!("also known as {}", self.aliases.join(", "));
        }
        "OSV returned this advisory id with no summary".to_owned()
    }
}

/// One queried package and what OSV said about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PackageVulns {
    /// The package name.
    pub package: String,
    /// The OSV ecosystem it was queried under.
    pub ecosystem: String,
    /// The pinned version queried.
    pub version: String,
    /// Every advisory OSV returned, worst band first.
    pub vulns: Vec<Vuln>,
}

/// The `osv.json` one repository's scan writes into the bundle.
///
/// `errors` is never elided: an empty `packages` beside a populated `errors` is
/// the state a reader must be able to tell apart from a clean scan.
/// Test: `osv_tests::the_scan_lands_in_the_bundle`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Scan {
    /// Coordinates actually sent to OSV or answered from cache.
    pub queried: usize,
    /// Coordinates that came back carrying at least one advisory.
    pub matched: usize,
    /// One line per coordinate or batch that did not produce an answer.
    pub errors: Vec<String>,
    /// One entry per matched package.
    pub packages: Vec<PackageVulns>,
}

/// Inventory ecosystem labels, and the OSV ecosystem each maps onto.
///
/// The left column is lowercased before lookup, so it covers both the four
/// labels `trusty-review`'s inventory emits today (`cargo`, `npm`, `pypi`,
/// `go`) and the spellings a future producer is likely to use. The right column
/// is OSV's own name and is case-sensitive there.
pub const OSV_ECOSYSTEMS: &[(&str, &str)] = &[
    ("cargo", "crates.io"),
    ("crates", "crates.io"),
    ("crates.io", "crates.io"),
    ("rust", "crates.io"),
    ("npm", "npm"),
    ("node", "npm"),
    ("pypi", "PyPI"),
    ("pip", "PyPI"),
    ("python", "PyPI"),
    ("go", "Go"),
    ("golang", "Go"),
    ("maven", "Maven"),
    ("gradle", "Maven"),
    ("java", "Maven"),
    ("rubygems", "RubyGems"),
    ("gem", "RubyGems"),
    ("ruby", "RubyGems"),
    ("nuget", "NuGet"),
    ("dotnet", "NuGet"),
];

/// The OSV ecosystem an inventory label names, when OSV covers it.
///
/// `None` is a coordinate this leg cannot query, and the caller owes the report
/// a line naming the label rather than dropping the row — an ecosystem OSV does
/// not cover here is unassessed, not clean.
/// Test: `osv_tests::every_inventory_ecosystem_maps_to_an_osv_name`.
#[must_use]
pub fn osv_ecosystem(label: &str) -> Option<&'static str> {
    let key = label.trim().to_ascii_lowercase();
    OSV_ECOSYSTEMS
        .iter()
        .find(|(inventory, _)| *inventory == key)
        .map(|(_, osv)| *osv)
}

/// The dependency inventory one repository's snapshot carries.
///
/// Why: the three counts are what separate "OSV found nothing" from "OSV was
/// asked about a third of this repository". Only `coordinates` is queryable;
/// the other three fields are each a gap line the caller must state.
/// What: the queryable triples, how many rows the snapshot listed, how many the
/// producer measured before its own cap, and the rows that could not become a
/// coordinate.
/// Test: `osv_tests::{the_inventory_becomes_osv_coordinates,
/// a_capped_inventory_says_how_much_it_left_out}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Inventory {
    /// Rows that are pinned and in an ecosystem OSV covers.
    pub coordinates: Vec<Coordinate>,
    /// Rows the snapshot actually listed.
    pub listed: usize,
    /// Rows the producer measured before applying its own row cap.
    pub declared: usize,
    /// Rows with no locked version, named — OSV cannot answer a constraint.
    pub unpinned: Vec<String>,
    /// Rows in an ecosystem [`osv_ecosystem`] does not map, named.
    pub unmapped: Vec<String>,
}

/// Read the dependency inventory `trusty-review` measured for this repository.
///
/// Why: the inventory is already on disk. Re-deriving it here would be a second
/// implementation of another crate's capability, and re-parsing lock files is
/// the exact duplication CLAUDE.md's common-entry-point rule forbids.
/// What: `repos[].deps.{deps,total}` from the snapshot, folded across every
/// repository entry the file carries — on the sweep path that is the one
/// repository the directory belongs to. A row contributes a coordinate when it
/// has a `locked` version AND a mappable ecosystem, and is named in `unpinned`
/// or `unmapped` otherwise.
///
/// # Errors
/// One line, safe to show the recipient, when the snapshot is absent,
/// unreadable, or not JSON. An absent snapshot is the ordinary shape of a run
/// whose child failed before it rendered.
///
/// Test: `osv_tests::{the_inventory_becomes_osv_coordinates,
/// a_missing_snapshot_is_a_named_gap}`.
pub fn inventory(snapshot: &Path) -> Result<Inventory, String> {
    let text = std::fs::read_to_string(snapshot).map_err(|e| {
        format!(
            "{COLLECTOR}: the dependency inventory at {} could not be read ({e})",
            snapshot.display()
        )
    })?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        format!(
            "{COLLECTOR}: {} is not readable as JSON ({e})",
            snapshot.display()
        )
    })?;
    let repos = doc
        .get("repos")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            format!(
                "{COLLECTOR}: {} declares no `repos` array, so it is not an investigation snapshot",
                snapshot.display()
            )
        })?;

    let mut inventory = Inventory::default();
    for repo in repos {
        let Some(deps) = repo.get("deps") else {
            continue;
        };
        inventory.declared += deps
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default() as usize;
        for row in deps
            .get("deps")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            inventory.listed += 1;
            absorb(&mut inventory, row);
        }
    }
    inventory.coordinates.sort();
    inventory.coordinates.dedup();
    Ok(inventory)
}

/// One inventory row: a coordinate, or the reason it is not one.
fn absorb(inventory: &mut Inventory, row: &serde_json::Value) {
    let string = |key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let (Some(name), Some(label)) = (string("name"), string("ecosystem")) else {
        return;
    };
    let Some(ecosystem) = osv_ecosystem(label) else {
        inventory.unmapped.push(format!("{name} ({label})"));
        return;
    };
    match string("locked") {
        Some(version) => inventory
            .coordinates
            .push(Coordinate::new(ecosystem, name, version)),
        // A declared constraint is not a version. OSV answers `1.2.3`, never
        // `^1.2`, so guessing which release satisfies it here would invent the
        // fact the whole leg exists to measure.
        None => inventory.unpinned.push(format!("{name} ({label})")),
    }
}

/// Split `coordinates` into calls OSV will accept.
///
/// Why: OSV documents 1000 queries per `querybatch` call and rejects more, so
/// an engagement with a large inventory is several calls rather than one that
/// fails. Split out so the boundary is testable without a request.
/// What: consecutive chunks of at most [`MAX_QUERIES_PER_BATCH`], in order.
/// Test: `osv_tests::batches_split_at_the_query_cap`.
#[must_use]
pub fn batches(coordinates: &[Coordinate]) -> Vec<&[Coordinate]> {
    coordinates.chunks(MAX_QUERIES_PER_BATCH.max(1)).collect()
}

/// Scan one repository's inventory, or say why there is no scan.
///
/// Why/What: see the module docs. Reads no lock file and starts no subprocess —
/// the inventory is the snapshot's, and the only I/O is the cache and (unless
/// [`Settings::offline`]) the batch endpoint.
///
/// # Postconditions
/// Never panics and never returns an error. Every degradation is a line in the
/// returned gap list AND in `osv.json`'s `errors`, naming `display`.
///
/// Test: `osv_tests`.
pub async fn ground_into(manifest: &Path, settings: &Settings, display: &str) -> Vec<String> {
    let Some(dir) = manifest.parent() else {
        return vec![format!(
            "{display}: {COLLECTOR}: {} has no parent directory, so this repository's OSV scan had \
             nowhere to write",
            manifest.display()
        )];
    };
    let inventory = match inventory(&dir.join(INVENTORY_FILE)) {
        Ok(inventory) => inventory,
        Err(cause) => {
            return vec![format!(
                "{display}: {cause} — no OSV lookup ran for it, which must be read as unassessed \
                 rather than as a dependency set with no known vulnerabilities"
            )];
        }
    };

    let mut gaps = coverage_gaps(&inventory, display);
    if inventory.coordinates.is_empty() {
        gaps.push(format!(
            "{display}: {COLLECTOR}: its dependency inventory offered no pinned package in an \
             ecosystem OSV covers, so no query ran"
        ));
        return gaps;
    }

    let (answers, errors) = osv_query::resolve(settings, &inventory.coordinates).await;
    let scan = assemble(&inventory.coordinates, &answers, errors);
    if let Err(cause) = write_scan(&dir.join(SCAN_FILE), &scan) {
        gaps.push(format!("{display}: {COLLECTOR}: {cause}"));
    }
    gaps.extend(outcome_gaps(&scan, &inventory, display));
    if let Err(cause) = write_into(manifest, &scan) {
        gaps.push(format!(
            "{display}: {COLLECTOR}: {cause} — the report states none of the advisories OSV \
             returned for it, though `{SCAN_FILE}` in this repository's directory carries them"
        ));
    }
    gaps
}

/// The lines the inventory itself owes the report, before any query.
fn coverage_gaps(inventory: &Inventory, display: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    if inventory.declared > inventory.listed {
        gaps.push(format!(
            "{display}: {COLLECTOR}: the dependency inventory this scan reads lists {} of the {} \
             package(s) the report measured — the renderer caps it — so {} were never offered to \
             OSV",
            inventory.listed,
            inventory.declared,
            inventory.declared - inventory.listed
        ));
    }
    if !inventory.unpinned.is_empty() {
        gaps.push(format!(
            "{display}: {COLLECTOR}: {} package(s) declare no locked version, so OSV could not be \
             asked about them: {}",
            inventory.unpinned.len(),
            named(&inventory.unpinned)
        ));
    }
    if !inventory.unmapped.is_empty() {
        gaps.push(format!(
            "{display}: {COLLECTOR}: {} package(s) are in an ecosystem this collector does not map \
             onto OSV, so they are unassessed: {}",
            inventory.unmapped.len(),
            named(&inventory.unmapped)
        ));
    }
    gaps
}

/// What the completed scan owes the report.
fn outcome_gaps(scan: &Scan, inventory: &Inventory, display: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    let asked = inventory.coordinates.len();
    if scan.queried == 0 {
        gaps.push(format!(
            "{display}: {COLLECTOR}: none of the {asked} package(s) offered to OSV produced an \
             answer ({}) — this repository has no OSV coverage at all, which must be read as \
             unassessed rather than clean",
            named(&scan.errors)
        ));
        return gaps;
    }
    for error in &scan.errors {
        gaps.push(format!("{display}: {COLLECTOR}: {error}"));
    }
    if scan.packages.is_empty() {
        gaps.push(format!(
            "{display}: {COLLECTOR}: OSV was asked about {} of this repository's {asked} pinned \
             package(s) and returned no advisory. Transitive dependencies, vendored code and \
             build-time downloads are outside that inventory",
            scan.queried
        ));
    }
    gaps
}

/// Up to five names, then a count — a gap line is one line.
fn named(items: &[String]) -> String {
    const SHOWN: usize = 5;
    if items.len() <= SHOWN {
        return items.join("; ");
    }
    format!(
        "{}; and {} more",
        items[..SHOWN].join("; "),
        items.len() - SHOWN
    )
}

/// Fold the per-coordinate answers into the document the bundle carries.
///
/// An answer of `None` is a coordinate nothing answered for; it is already
/// named in `errors` by [`osv_query::resolve`] and contributes to neither
/// `queried` nor `matched`.
/// Test: `osv_tests::the_scan_lands_in_the_bundle`.
#[must_use]
pub fn assemble(
    coordinates: &[Coordinate],
    answers: &[Option<Vec<Vuln>>],
    errors: Vec<String>,
) -> Scan {
    let mut scan = Scan {
        errors,
        ..Scan::default()
    };
    for (coordinate, answer) in coordinates.iter().zip(answers) {
        let Some(vulns) = answer else { continue };
        scan.queried += 1;
        if vulns.is_empty() {
            continue;
        }
        scan.matched += 1;
        let mut vulns = vulns.clone();
        vulns.sort_by(|a, b| a.severity.cmp(&b.severity).then_with(|| a.id.cmp(&b.id)));
        scan.packages.push(PackageVulns {
            package: coordinate.name.clone(),
            ecosystem: coordinate.ecosystem.clone(),
            version: coordinate.version.clone(),
            vulns,
        });
    }
    scan
}

/// Write `osv.json` into the repository's bundle directory.
///
/// # Errors
/// One line when the file cannot be serialised or written.
pub fn write_scan(path: &Path, scan: &Scan) -> Result<(), String> {
    let json = serde_json::to_string_pretty(scan)
        .map_err(|e| format!("the OSV result could not be serialised ({e})"))?;
    std::fs::write(path, format!("{json}\n"))
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// The keys that identify one declared row, for the resumed-sweep skip.
const IDENTITY: &[&str] = &["id", "package", "version"];

/// Record the scan's advisories in the manifest as `[report].findings`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19), and rows this
/// process holds and does not write reach no renderer.
/// What: one row per (package, advisory) pair, through
/// [`super::findings::append`] — the shared writer, which skips a row already
/// declared under the same id/package/version so a resumed sweep does not
/// restate it. An empty scan writes nothing, for the reason stated there.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back.
///
/// Test: `osv_tests::the_advisories_land_in_the_manifest`.
pub fn write_into(manifest: &Path, scan: &Scan) -> Result<(), String> {
    let rows: Vec<InlineTable> = scan
        .packages
        .iter()
        .flat_map(|package| package.vulns.iter().map(|vuln| row(package, vuln)))
        .collect();
    super::findings::append(manifest, &rows, IDENTITY)
}

/// One advisory as the inline table `trusty-review` deserialises.
fn row(package: &PackageVulns, vuln: &Vuln) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("category", Value::from(CATEGORY));
    table.insert("id", Value::from(vuln.id.as_str()));
    table.insert("package", Value::from(package.package.as_str()));
    table.insert("version", Value::from(package.version.as_str()));
    table.insert("severity", Value::from(vuln.severity.band().as_str()));
    table.insert("title", Value::from(vuln.title()));
    table.insert("url", Value::from(vuln.url()));
    table
}

#[cfg(test)]
#[path = "osv_tests.rs"]
mod osv_tests;
