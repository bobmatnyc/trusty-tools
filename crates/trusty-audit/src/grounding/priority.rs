//! Writing the ranking, and the gaps, into the manifest (#6081, #6078).
//!
//! Why: the manifest is the interface (owner ruling 2026-08-19). trusty-review
//! reads `inspect_priority` off each `[[repositories]]` entry and inspects those
//! files ahead of its own path-name heuristics, and it reads `[report].gaps`
//! into the report's Gaps & Caveats section. A ranking this process holds and
//! does not write reaches no renderer — not `tga audit`'s, not
//! `crate::rerender`'s, and not the one the recipient runs over the delivered
//! package, which is the whole reason it goes in the file that ships.
//!
//! What: [`write_into`], a surgical `toml_edit` update of an existing manifest.
//! `toml_edit` rather than a `toml::Value` round trip because the file is
//! written by `tga` and read by `trusty-review`, and rewriting every key of a
//! document two other crates own — reordering it, dropping its comments — to add
//! one key is a much larger claim than this change is making.
//!
//! ## Which entry, and why by path
//!
//! The entry is matched on its `path`, never its `name`. The sweep generates
//! `tga`'s config with a filename-safe STEM as the repository name while the
//! operator sees the registered name, so matching on a name means agreeing with
//! a derivation this module cannot see. The checkout path is the same value on
//! both sides by construction — it is what was indexed.
//!
//! Test: `priority_tests`.

use std::path::Path;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

/// One ranked path, and what put it there (#6082).
///
/// Why: a ranking that only says WHICH files to read cannot tell the report why
/// any of them was read, and "why" is the whole difference between a coverage
/// section that states a percentage and one a due-diligence reader can weigh.
/// The three optional fields are what trusty-review renders per dimension.
/// What: `path` is repo-relative; `dimension` is a DD dimension spelled as
/// trusty-review spells it (absent for a signal that belongs to no single
/// dimension, such as a complexity hotspot); `reason` is one line naming the
/// query or measurement that selected it; `hotspot` is the file's worst
/// measured function, when the complexity leg ranked this file (#6145).
/// Test: `priority_tests::{a_dimension_and_reason_are_written_as_a_table,
/// a_hotspot_is_written_as_a_nested_table}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Priority {
    /// Repo-relative path to inspect.
    pub path: String,
    /// The DD dimension this file is evidence for, when it is evidence for one.
    pub dimension: Option<String>,
    /// One line naming the query or measurement that selected it.
    pub reason: Option<String>,
    /// The file's hottest function, when trusty-analyze measured one (#6145).
    pub hotspot: Option<FunctionHotspot>,
}

impl Priority {
    /// A ranked path with no attribution — the shape #6081 wrote.
    #[must_use]
    pub fn bare(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }
}

/// The worst measured function inside one ranked file (#6145).
///
/// Why: `/complexity_hotspots` ranks FUNCTIONS and the manifest names FILES, so
/// collapsing chunks to files discarded the only part of the measurement that
/// says WHERE in a 900-line file the complexity is. The owner ruled that
/// analysis must target the most complex functions, and a path alone cannot
/// carry that instruction to the crate that acts on it (trusty-review, #6146).
/// What: the winning chunk's own line range and cyclomatic count, plus its name
/// when the daemon supplied one. The range is what makes it actionable; the
/// name is what makes it readable, and a chunk the parser could not name still
/// has a usable range.
/// Test: `super::hotspot_tests::the_hottest_function_of_each_file_is_kept`,
/// `priority_tests::a_hotspot_is_written_as_a_nested_table`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct FunctionHotspot {
    /// The function's name, when trusty-analyze named it.
    pub function: Option<String>,
    /// First line of the measured chunk, 1-based.
    pub start_line: u32,
    /// Last line of the measured chunk, 1-based and inclusive.
    pub end_line: u32,
    /// The chunk's cyclomatic complexity — the number that ranked it.
    pub cyclomatic: u32,
}

/// The investigation budget the audit asks for, in files and content bytes.
///
/// Why: trusty-review's own defaults (40 files / 400 KiB) read about 1% of a
/// workspace-sized repository, which the owner ruled too shallow for a DD
/// report (#6082). The budget is a manifest key, so raising it is one more
/// thing written to the interface rather than a flag the operator must learn —
/// `trusty-audit audit` still takes no new step.
/// What: written to `[report]` PER KEY, and only where the manifest declares
/// none — an operator who set one of the two keeps it and gets the audit's
/// default for the other, rather than falling back to trusty-review's. The two
/// numbers are not independent: trusty-review's selection stops at whichever
/// binds first, so `max_bytes` is DERIVED from `max_files` unless something
/// declares it (#6148).
/// Test: `priority_tests::{the_budget_is_recorded_once,
/// a_declared_budget_is_left_alone, a_raised_file_budget_raises_the_byte_budget}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Budget {
    /// Files the investigation pass may read.
    pub max_files: usize,
    /// Total content bytes it may send.
    pub max_bytes: usize,
}

/// Files the audit asks the investigation pass to read per repository.
///
/// 240 rather than the 120 #6082 shipped: the owner's ruling is that a DD
/// sample reads above 1% of the repository, and lap-2 measured 120 at roughly
/// 1.8% of this workspace (#6148).
pub const DEFAULT_MAX_FILES: usize = 240;

/// Content bytes one budgeted file is assumed to carry.
///
/// The ratio the 120-file / 1.2 MiB pair shipped with, kept as the derivation
/// constant so the two knobs move together.
pub const BYTES_PER_FILE: usize = 10 * 1024;

/// Content bytes the audit asks it to send per repository.
pub const DEFAULT_MAX_BYTES: usize = DEFAULT_MAX_FILES * BYTES_PER_FILE;

/// Environment override for [`DEFAULT_MAX_FILES`].
///
/// Spelled once in `trusty_common::env_vars` (#6082) because trusty-review reads
/// the same variable — see [`Budget::child_env`].
pub const ENV_MAX_FILES: &str = trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_FILES;

/// Environment override for [`DEFAULT_MAX_BYTES`].
pub const ENV_MAX_BYTES: &str = trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_BYTES;

impl Budget {
    /// The budget THIS manifest runs under: what it declares, else the machine's.
    ///
    /// Why: the effective budget has two possible sources — a key an operator
    /// already wrote into `[report]`, which [`write_into`] leaves alone, and
    /// [`Budget::from_env`]. The evidence caps scale with the budget (#6082), so
    /// reading it anywhere but here would let the caps and the manifest disagree
    /// on a manifest that declares one: the caps would size for 120 while
    /// trusty-review read 300. One resolver, one value, both consumers.
    /// What: a positive `[report].investigate_max_files` /
    /// `investigate_max_bytes` wins per key, then the environment override for
    /// that key, then the default — and an absent byte value derives from
    /// whichever file value won, so raising files in the manifest raises bytes
    /// with it. An unreadable manifest and an unusable value both behave as if
    /// the key were absent.
    /// Test: `priority_tests::{a_declared_budget_is_the_effective_budget,
    /// a_manifest_file_budget_raises_the_byte_budget}`.
    #[must_use]
    pub fn for_manifest(manifest: &Path) -> Self {
        let declared = std::fs::read_to_string(manifest)
            .ok()
            .and_then(|text| text.parse::<toml::Value>().ok());
        let key = |name: &str| -> Option<usize> {
            let value = declared.as_ref()?.get("report")?.get(name)?.as_integer()?;
            usize::try_from(value).ok().filter(|n| *n > 0)
        };
        Self::resolve(
            key("investigate_max_files").or_else(|| env_positive(ENV_MAX_FILES)),
            key("investigate_max_bytes").or_else(|| env_positive(ENV_MAX_BYTES)),
        )
    }

    /// The budget this machine asks for, defaults unless overridden.
    #[must_use]
    pub fn from_env() -> Self {
        Self::resolve(env_positive(ENV_MAX_FILES), env_positive(ENV_MAX_BYTES))
    }

    /// The budget THIS ENGAGEMENT runs under, resolved once for the sweep.
    ///
    /// Why (#6247): before this, the value the child ran under came from
    /// [`Budget::from_env`] inside the spawn, and the value written into the
    /// delivered manifest came from [`Budget::for_manifest`] afterwards. Two
    /// resolutions of one number, and an operator had no way to declare it at
    /// all — so a run could hand back a manifest naming a budget its own
    /// investigation pass never used. `crate::run::sweep` now resolves here,
    /// once, and hands the SAME value to every child and to every manifest it
    /// records, which is what makes the two agree by construction rather than
    /// by both happening to fall through to the same tier.
    /// What: a positive declared key wins per key, then the environment
    /// override for that key, then the default — the same ladder
    /// [`Budget::for_manifest`] applies to a manifest's own keys, so an
    /// operator moving a value between the two files gets the same answer. A
    /// zero or absent value reads as undeclared rather than as a disabled
    /// investigation.
    /// Test: `priority_tests::{a_declared_engagement_budget_wins,
    /// an_engagement_declaring_nothing_matches_the_machine}`.
    #[must_use]
    pub fn for_engagement(settings: &crate::config::ReportSettings) -> Self {
        let declared = |value: Option<usize>| value.filter(|n| *n > 0);
        Self::resolve(
            declared(settings.investigate_max_files).or_else(|| env_positive(ENV_MAX_FILES)),
            declared(settings.investigate_max_bytes).or_else(|| env_positive(ENV_MAX_BYTES)),
        )
    }

    /// This budget as the environment pairs a `tga audit` child passes down.
    ///
    /// Why (#6082): the manifest is the interface, and on the sweep path the
    /// manifest arrives too late. `tga audit` writes the manifest and then, in
    /// the SAME process, runs `trusty-review report` against it; the audit's
    /// grounding pass only edits the file once that child has exited. So the
    /// 240-file budget [`write_into`] records reaches a re-render and never the
    /// report the sweep itself produces — the 2026-08-22 dogfood run declared
    /// `investigate_max_files = 240` in its manifest over an investigation whose
    /// own snapshot recorded `{"max_files": 40}`, trusty-review's bare default.
    /// The environment reaches the grandchild BEFORE the file does.
    /// What: both variables, always both, so a raised file budget never meets an
    /// unraised byte budget (#6148). It is the lowest tier trusty-review
    /// consults — an explicit `--investigate-max-files` and a manifest key both
    /// still win — so this adds a floor and overrides nothing.
    /// Test: `priority_tests::the_child_environment_carries_both_halves`.
    #[must_use]
    pub fn child_env(self) -> [(&'static str, String); 2] {
        [
            (ENV_MAX_FILES, self.max_files.to_string()),
            (ENV_MAX_BYTES, self.max_bytes.to_string()),
        ]
    }

    /// [`Budget::from_env`]'s rule, as a pure function.
    ///
    /// Why: reading the two variables is all `from_env` does, so the RULE —
    /// an unparseable or zero override falls back rather than disabling the
    /// investigation — is testable without `std::env::set_var`, which is
    /// `unsafe` in edition 2024 and unsound under the parallel harness.
    /// What: a positive integer wins; anything else takes the default, and an
    /// absent byte override derives from the file count rather than pinning
    /// [`DEFAULT_MAX_BYTES`].
    /// Test: `priority_tests::{an_unusable_override_falls_back_to_the_default,
    /// a_raised_file_budget_raises_the_byte_budget}`.
    #[must_use]
    pub fn resolved(files: Option<&str>, bytes: Option<&str>) -> Self {
        Self::resolve(positive(files), positive(bytes))
    }

    /// The one resolution rule, over values both sources have already parsed.
    ///
    /// Why: trusty-review's selection stops at whichever of the two caps binds
    /// first, so a raised file budget against a fixed byte budget reads fewer
    /// files than it asked for and says nothing — lap-2 read 76 of 120 (#6148).
    /// What: an explicit byte value always wins; otherwise bytes are
    /// [`BYTES_PER_FILE`] times whatever file count won, so the default pair
    /// stays exactly [`DEFAULT_MAX_FILES`] / [`DEFAULT_MAX_BYTES`].
    fn resolve(files: Option<usize>, bytes: Option<usize>) -> Self {
        let max_files = files.unwrap_or(DEFAULT_MAX_FILES);
        Self {
            max_files,
            max_bytes: bytes.unwrap_or_else(|| max_files.saturating_mul(BYTES_PER_FILE)),
        }
    }
}

/// A declared override, when it parses as a positive count.
fn positive(value: Option<&str>) -> Option<usize> {
    value?.trim().parse::<usize>().ok().filter(|n| *n > 0)
}

/// A positive environment override for `name`, when one is set.
fn env_positive(name: &str) -> Option<usize> {
    positive(std::env::var(name).ok().as_deref())
}

/// Record `priorities`, `budget` and `gaps` in the manifest at `path`.
///
/// Why gaps are written even when the ranking is not: a degraded leg is the one
/// thing that MUST reach the report. The two are independent — `[report].gaps`
/// belongs to the run, `inspect_priority` to one repository — so a gap is
/// recorded whether or not the repository entry can be found.
///
/// Why the BUDGET is written even when the ranking is not (#6149): the budget is
/// CONFIGURATION, not evidence. It used to sit inside the `priorities`
/// non-empty branch, so a run whose grounding legs degraded wrote its gaps and
/// nothing else — and trusty-review, finding no `investigate_max_files`, fell
/// back to its own 40-file default. The evidence failure silently took the
/// investigation depth with it: exactly the compounding the confirmation run of
/// 2026-08-21 hit, where the index collision cost the grounding AND left the
/// 240-file budget unwritten.
/// What: appends each gap to `[report].gaps`, skipping duplicates so a resumed
/// sweep does not restate them, fills the investigation budget keys when the
/// manifest declares none, and replaces the matched repository's
/// `inspect_priority` with the ranking. Nothing is written when there is no
/// ranking, no gap and no budget to record.
///
/// # Errors
///
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, matched, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, every line of `gaps` is in `[report].gaps` and — when `priorities`
/// is non-empty — the repository whose `path` is `checkout` declares exactly
/// those paths, in that order.
///
/// Test: `priority_tests::{the_ranking_lands_on_the_matching_repository,
/// gaps_are_appended_without_duplicating, a_ranking_with_no_matching_entry_is_refused}`.
pub fn write_into(
    path: &Path,
    checkout: &Path,
    priorities: &[Priority],
    budget: Option<Budget>,
    attributed_only: bool,
    gaps: &[String],
) -> Result<(), String> {
    if priorities.is_empty() && gaps.is_empty() && budget.is_none() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", path.display()))?;

    record_gaps(&mut doc, gaps)?;
    // #6149: configuration first, and unconditionally — a degraded evidence leg
    // must not silently drop the investigation budget with it.
    if let Some(budget) = budget {
        record_budget(&mut doc, budget)?;
    }
    if !priorities.is_empty() {
        record_priorities(&mut doc, checkout, priorities)?;
        if attributed_only {
            record_attributed_only(&mut doc)?;
        }
    }

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// Declare that the ranking IS the intended sample (#6082).
///
/// Why: `inspect_priority` is a dominant sort key in trusty-review, not a
/// filter, so a list shorter than the file budget is topped up with path-name
/// heuristics — unattributed files counted alongside evidence a query found.
/// The owner's ruling (2026-08-20) is that the filter is search, the knowledge
/// graph, and measured complexity; heuristics are the degradation path, not the
/// remainder. Writing this key is what tells the reader that.
/// What: sets `[report].attributed_only = true`, leaving a declared value alone
/// exactly as [`record_budget`] does. Written ONLY when the search leg produced
/// evidence — a ranking that is complexity-only, or a hand-written manifest, has
/// no business suppressing the heuristics that are then all it has.
/// Test: `priority_tests::attributed_only_is_declared_when_the_search_leg_worked`.
fn record_attributed_only(doc: &mut DocumentMut) -> Result<(), String> {
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    if table.get("attributed_only").is_none() {
        table.insert("attributed_only", Item::Value(Value::from(true)));
    }
    Ok(())
}

/// Fill `[report]`'s investigation budget keys, leaving declared ones alone.
fn record_budget(doc: &mut DocumentMut, budget: Budget) -> Result<(), String> {
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    for (key, value) in [
        ("investigate_max_files", budget.max_files),
        ("investigate_max_bytes", budget.max_bytes),
    ] {
        if table.get(key).is_none() {
            table.insert(
                key,
                Item::Value(Value::from(i64::try_from(value).unwrap_or(i64::MAX))),
            );
        }
    }
    Ok(())
}

/// Append each gap to `[report].gaps`, creating the key when it is absent.
fn record_gaps(doc: &mut DocumentMut, gaps: &[String]) -> Result<(), String> {
    if gaps.is_empty() {
        return Ok(());
    }
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    let item = table
        .entry("gaps")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())));
    let array = item
        .as_array_mut()
        .ok_or_else(|| "the manifest's `report.gaps` is not an array".to_string())?;
    for gap in gaps {
        if !array.iter().any(|v| v.as_str() == Some(gap.as_str())) {
            array.push(gap.as_str());
        }
    }
    Ok(())
}

/// Declare `priorities` on the `[[repositories]]` entry whose path is `checkout`.
fn record_priorities(
    doc: &mut DocumentMut,
    checkout: &Path,
    priorities: &[Priority],
) -> Result<(), String> {
    let repositories = doc
        .get_mut("repositories")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "the manifest declares no `[[repositories]]` entry".to_string())?;
    let entry = repositories
        .iter_mut()
        .find(|table| names_checkout(table, checkout))
        .ok_or_else(|| {
            format!(
                "no `[[repositories]]` entry names the checkout at {}",
                checkout.display()
            )
        })?;
    entry.insert(
        "inspect_priority",
        Item::Value(Value::Array(ranked(priorities))),
    );
    Ok(())
}

/// Whether this repository entry's `path` is the checkout that was indexed.
///
/// Compared as written first, then through `canonicalize`, so a manifest naming
/// the same directory by a symlinked or non-normalised path still matches. A
/// path that cannot be canonicalised — the checkout has since been deleted —
/// falls back to the textual comparison rather than erroring.
pub(super) fn names_checkout(entry: &Table, checkout: &Path) -> bool {
    let Some(declared) = entry.get("path").and_then(Item::as_str) else {
        return false;
    };
    let declared = Path::new(declared);
    if declared == checkout {
        return true;
    }
    match (declared.canonicalize(), checkout.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The ranking as a multi-line TOML array.
///
/// No `weight` key on purpose: trusty-review derives each entry's weight from
/// its DECLARED POSITION (#6078's `PRIORITY_BASE_WEIGHT` rule), so a rank
/// expressed as order needs no agreement about a numeric scale that lives in
/// another crate and can move. An entry with no attribution stays a bare string
/// — the #6081 shape, and still what trusty-review reads for a hand-written
/// manifest.
fn ranked(priorities: &[Priority]) -> Array {
    let mut array = Array::new();
    for priority in priorities {
        let mut value = entry(priority);
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    array
}

/// One priority as TOML: a bare path, or a table when it carries attribution.
fn entry(priority: &Priority) -> Value {
    if priority.dimension.is_none() && priority.reason.is_none() && priority.hotspot.is_none() {
        return Value::from(priority.path.as_str());
    }
    let mut table = InlineTable::new();
    table.insert("path", Value::from(priority.path.as_str()));
    if let Some(dimension) = &priority.dimension {
        table.insert("dimension", Value::from(dimension.as_str()));
    }
    if let Some(reason) = &priority.reason {
        table.insert("reason", Value::from(reason.as_str()));
    }
    // #6145: nested rather than four `hotspot_*` siblings, so the measurement
    // reads as one fact and a reader can tell at a glance which keys came from
    // the complexity leg.
    if let Some(hotspot) = &priority.hotspot {
        table.insert("hotspot", Value::InlineTable(hotspot_table(hotspot)));
    }
    Value::InlineTable(table)
}

/// One measured function as a nested inline table (#6145).
fn hotspot_table(hotspot: &FunctionHotspot) -> InlineTable {
    let mut table = InlineTable::new();
    if let Some(function) = &hotspot.function {
        table.insert("function", Value::from(function.as_str()));
    }
    for (key, value) in [
        ("start_line", hotspot.start_line),
        ("end_line", hotspot.end_line),
        ("cyclomatic", hotspot.cyclomatic),
    ] {
        table.insert(key, Value::from(i64::from(value)));
    }
    table
}

#[cfg(test)]
mod priority_tests {
    use super::*;

    /// Shaped after what `tga::report::dd_manifest::DdManifest::to_toml` emits.
    const SAMPLE: &str = r#"[report]
title = "Acme — Technical Due Diligence"
gaps = ["Two repositories could not be cloned."]

[[repositories]]
name = "01-acme-api"
path = "/w/repos/acme-api"

[[repositories]]
name = "02-acme-web"
path = "/w/repos/acme-web"
"#;

    fn written(text: &str, checkout: &str, priorities: &[&str], gaps: &[&str]) -> String {
        let priorities: Vec<Priority> = priorities.iter().map(|s| Priority::bare(*s)).collect();
        recorded(text, checkout, &priorities, None, gaps)
    }

    fn recorded(
        text: &str,
        checkout: &str,
        priorities: &[Priority],
        budget: Option<Budget>,
        gaps: &[&str],
    ) -> String {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, text).expect("write");
        let gaps: Vec<String> = gaps.iter().map(|s| (*s).to_owned()).collect();
        write_into(&path, Path::new(checkout), priorities, budget, false, &gaps).expect("records");
        std::fs::read_to_string(&path).expect("read back")
    }

    fn attributed(path: &str, dimension: &str, reason: &str) -> Priority {
        Priority {
            path: path.to_owned(),
            dimension: Some(dimension.to_owned()),
            reason: Some(reason.to_owned()),
            hotspot: None,
        }
    }

    /// #6145: the complexity leg's entry — a path and the function it measured,
    /// with no dimension, which is the shape `evidence::blend` produces.
    #[test]
    fn a_hotspot_is_written_as_a_nested_table() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[Priority {
                path: "src/pay.rs".to_owned(),
                dimension: None,
                reason: Some("trusty-analyze complexity hotspot (rank 1)".to_owned()),
                hotspot: Some(FunctionHotspot {
                    function: Some("settle_invoice".to_owned()),
                    start_line: 40,
                    end_line: 190,
                    cyclomatic: 31,
                }),
            }],
            None,
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let first = &parsed["repositories"].as_array().expect("array")[0]["inspect_priority"]
            .as_array()
            .expect("declared")[0];
        assert_eq!(first["path"].as_str(), Some("src/pay.rs"), "{out}");
        let hotspot = &first["hotspot"];
        assert_eq!(
            hotspot["function"].as_str(),
            Some("settle_invoice"),
            "{out}"
        );
        assert_eq!(hotspot["start_line"].as_integer(), Some(40), "{out}");
        assert_eq!(hotspot["end_line"].as_integer(), Some(190), "{out}");
        assert_eq!(hotspot["cyclomatic"].as_integer(), Some(31), "{out}");
    }

    /// #6145: an unnamed chunk writes its range and omits the key rather than
    /// declaring a function called "".
    #[test]
    fn an_unnamed_hotspot_omits_the_function_key() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[Priority {
                path: "src/pay.rs".to_owned(),
                hotspot: Some(FunctionHotspot {
                    function: None,
                    start_line: 4,
                    end_line: 44,
                    cyclomatic: 12,
                }),
                ..Priority::default()
            }],
            None,
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let hotspot = &parsed["repositories"].as_array().expect("array")[0]["inspect_priority"]
            .as_array()
            .expect("declared")[0]["hotspot"];
        assert!(hotspot.get("function").is_none(), "{out}");
        assert_eq!(hotspot["start_line"].as_integer(), Some(4), "{out}");
    }

    /// #6082: an attributed entry becomes a table carrying its dimension and the
    /// query that found it — the "why this file" the coverage section renders.
    #[test]
    fn a_dimension_and_reason_are_written_as_a_table() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[attributed(
                "src/auth.rs",
                "authentication & secrets",
                "trusty-search hit for \"credential handling\" (score 0.82, line 40)",
            )],
            None,
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let first = &parsed["repositories"].as_array().expect("array")[0]["inspect_priority"]
            .as_array()
            .expect("declared")[0];
        assert_eq!(first["path"].as_str(), Some("src/auth.rs"));
        assert_eq!(
            first["dimension"].as_str(),
            Some("authentication & secrets")
        );
        assert!(
            first["reason"]
                .as_str()
                .expect("reason")
                .contains("credential handling"),
            "{out}"
        );
    }

    /// An override that cannot be used must not turn the investigation off.
    #[test]
    fn an_unusable_override_falls_back_to_the_default() {
        assert_eq!(
            Budget::resolved(Some("0"), Some("not a number")),
            Budget {
                max_files: DEFAULT_MAX_FILES,
                max_bytes: DEFAULT_MAX_BYTES,
            }
        );
        assert_eq!(Budget::resolved(Some(" 12 "), None).max_files, 12);
    }

    /// #6148: the two caps bind together in trusty-review, so raising files
    /// alone used to read fewer files than it asked for — lap-2 read 76 of 120.
    #[test]
    fn a_raised_file_budget_raises_the_byte_budget() {
        let raised = Budget::resolved(Some("334"), None);
        assert_eq!(raised.max_files, 334);
        assert_eq!(
            raised.max_bytes, 3_420_160,
            "334 files earn 3.34 MiB, not the default cap"
        );
        assert!(
            raised.max_bytes > DEFAULT_MAX_BYTES,
            "a budget above the default must not be capped at it"
        );
    }

    /// An explicit byte override is the one thing derivation must not overrule.
    #[test]
    fn an_explicit_byte_override_wins_over_the_derivation() {
        let budget = Budget::resolved(Some("334"), Some("4096"));
        assert_eq!(budget.max_files, 334);
        assert_eq!(budget.max_bytes, 4096);
    }

    /// The default pair, and the ratio that ties them.
    #[test]
    fn the_default_budget_is_240_files_and_2_4_mib() {
        assert_eq!(DEFAULT_MAX_FILES, 240);
        assert_eq!(DEFAULT_MAX_BYTES, 2_457_600);
        assert_eq!(
            Budget::resolved(None, None),
            Budget {
                max_files: DEFAULT_MAX_FILES,
                max_bytes: DEFAULT_MAX_BYTES,
            }
        );
    }

    /// #6148, through the manifest: the same derivation, and the same
    /// precedence — a declared byte key still wins.
    #[test]
    fn a_manifest_file_budget_raises_the_byte_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        let with = |keys: &str| {
            SAMPLE.replace(
                "title = \"Acme — Technical Due Diligence\"",
                &format!("title = \"Acme\"\n{keys}"),
            )
        };

        std::fs::write(&path, with("investigate_max_files = 334")).expect("write");
        assert_eq!(
            Budget::for_manifest(&path).max_bytes,
            env_positive(ENV_MAX_BYTES).unwrap_or(3_420_160),
            "a declared file budget carries the byte budget with it"
        );

        std::fs::write(
            &path,
            with("investigate_max_files = 334\ninvestigate_max_bytes = 4096"),
        )
        .expect("write");
        assert_eq!(
            Budget::for_manifest(&path),
            Budget {
                max_files: 334,
                max_bytes: 4096,
            },
            "both declared keys win"
        );
    }

    /// #6082: the budget the audit asks for is recorded once, and only when the
    /// manifest is silent about it.
    #[test]
    fn the_budget_is_recorded_once() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[Priority::bare("src/pay.rs")],
            Some(Budget {
                max_files: 120,
                max_bytes: 1_200_000,
            }),
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
        assert_eq!(
            parsed["report"]["investigate_max_files"].as_integer(),
            Some(120)
        );
        assert_eq!(
            parsed["report"]["investigate_max_bytes"].as_integer(),
            Some(1_200_000)
        );
    }

    /// #6082: the budget also travels by environment, because the manifest key
    /// arrives too late on the sweep path — `tga audit` renders from the
    /// manifest in the same process that wrote it, and grounding edits that file
    /// only after the child exits. Both halves go, always: a raised file budget
    /// against an unraised byte budget reads fewer files than it asked for and
    /// says nothing (#6148). Fails against the pre-fix code, which had no such
    /// channel — the 2026-08-22 run declared `investigate_max_files = 240` in a
    /// manifest whose investigation recorded `max_files: 40`.
    #[test]
    fn the_child_environment_carries_both_halves() {
        let pairs = Budget {
            max_files: 240,
            max_bytes: 2_457_600,
        }
        .child_env();
        assert_eq!(
            pairs,
            [
                ("TRUSTY_AUDIT_INVESTIGATE_MAX_FILES", "240".to_owned()),
                ("TRUSTY_AUDIT_INVESTIGATE_MAX_BYTES", "2457600".to_owned()),
            ]
        );
        assert_eq!(
            pairs[0].0,
            trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_FILES,
            "trusty-review reads the same constant, so the spelling cannot drift"
        );
        assert_eq!(
            pairs[1].0,
            trusty_common::env_vars::ENV_AUDIT_INVESTIGATE_MAX_BYTES
        );
    }

    /// #6149: the budget is configuration, not evidence. A run whose grounding
    /// legs both failed produces gaps and no ranking, and it must STILL declare
    /// the budget — otherwise trusty-review falls back to its own 40-file
    /// default and the evidence failure quietly costs the investigation depth
    /// too. Fails against the pre-fix code, which wrote the budget only inside
    /// the non-empty-ranking branch.
    #[test]
    fn a_degraded_grounding_still_declares_the_budget() {
        let out = recorded(
            SAMPLE,
            "/w/repos/acme-api",
            &[],
            Some(Budget {
                max_files: DEFAULT_MAX_FILES,
                max_bytes: DEFAULT_MAX_BYTES,
            }),
            &[
                "acme-api: complexity data unavailable: index root mismatch",
                "acme-api: the search index matched no evidence for any dimension",
            ],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
        assert_eq!(
            parsed["report"]["investigate_max_files"].as_integer(),
            Some(240),
            "{out}"
        );
        assert_eq!(
            parsed["report"]["investigate_max_bytes"].as_integer(),
            Some(2_457_600),
            "{out}"
        );
        assert_eq!(parsed["report"]["gaps"].as_array().expect("array").len(), 3);
        assert!(
            parsed["repositories"].as_array().expect("array")[0]
                .get("inspect_priority")
                .is_none(),
            "a degraded run declares no ranking: {out}"
        );
    }

    /// An operator who declared a budget keeps it — the audit fills, never
    /// overrides — and it fills PER KEY, so declaring one of the two leaves the
    /// other at the audit's default rather than at trusty-review's.
    #[test]
    fn a_declared_budget_is_left_alone() {
        for (declared_key, declared_value) in [
            ("investigate_max_files", 7_i64),
            ("investigate_max_bytes", 4096),
        ] {
            let manifest = SAMPLE.replace(
                "title = \"Acme — Technical Due Diligence\"",
                &format!("title = \"Acme\"\n{declared_key} = {declared_value}"),
            );
            let out = recorded(
                &manifest,
                "/w/repos/acme-api",
                &[Priority::bare("src/pay.rs")],
                Some(Budget {
                    max_files: 120,
                    max_bytes: 1_200_000,
                }),
                &[],
            );
            let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
            assert_eq!(
                parsed["report"][declared_key].as_integer(),
                Some(declared_value),
                "the declared key survives: {out}"
            );
            let filled = if declared_key == "investigate_max_files" {
                ("investigate_max_bytes", 1_200_000)
            } else {
                ("investigate_max_files", 120)
            };
            assert_eq!(
                parsed["report"][filled.0].as_integer(),
                Some(filled.1),
                "the undeclared key still gets the audit's default: {out}"
            );
        }
    }

    /// #6082: the caps and the manifest read ONE budget. A manifest declaring a
    /// raised `investigate_max_files` is what trusty-review will actually read,
    /// so it is what the evidence caps must size for — resolving the budget from
    /// the environment here would size a 60-path ranking for a 300-file pass.
    #[test]
    fn a_declared_budget_is_the_effective_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        let declared = SAMPLE.replace(
            "title = \"Acme — Technical Due Diligence\"",
            "title = \"Acme\"\ninvestigate_max_files = 300",
        );
        std::fs::write(&path, &declared).expect("write");
        let budget = Budget::for_manifest(&path);
        assert_eq!(budget.max_files, 300, "the declared key wins");
        assert_eq!(
            budget.max_bytes,
            env_positive(ENV_MAX_BYTES).unwrap_or(300 * BYTES_PER_FILE),
            "the undeclared byte key derives from the declared file count (#6148)"
        );
        assert_eq!(
            crate::grounding::evidence::Caps::for_budget(budget.max_files).priority_paths,
            300,
            "the caps size for what the manifest declares"
        );

        // Silent manifest, absent manifest, and unusable value all fall back.
        std::fs::write(&path, SAMPLE).expect("write");
        assert_eq!(Budget::for_manifest(&path), Budget::from_env());
        std::fs::write(
            &path,
            SAMPLE.replace("[report]", "[report]\ninvestigate_max_files = 0"),
        )
        .expect("write");
        assert_eq!(Budget::for_manifest(&path), Budget::from_env());
        assert_eq!(
            Budget::for_manifest(&tmp.path().join("absent.toml")),
            Budget::from_env()
        );
    }

    /// A `[report]` section carrying whichever keys the arguments name.
    fn settings(files: Option<usize>, bytes: Option<usize>) -> crate::config::ReportSettings {
        let mut declared = String::new();
        if let Some(files) = files {
            declared.push_str(&format!("investigate_max_files = {files}\n"));
        }
        if let Some(bytes) = bytes {
            declared.push_str(&format!("investigate_max_bytes = {bytes}\n"));
        }
        toml::from_str(&declared).expect("parses")
    }

    /// 🔴 #6247: an engagement that declares a budget gets it, and gets it in
    /// both dimensions — the file count it named and a byte count derived from
    /// that count rather than pinned to the default.
    #[test]
    fn a_declared_engagement_budget_wins() {
        let budget = Budget::for_engagement(&settings(Some(77), None));
        assert_eq!(budget.max_files, 77, "the declared key wins");
        assert_eq!(
            budget.max_bytes,
            env_positive(ENV_MAX_BYTES).unwrap_or(77 * BYTES_PER_FILE),
            "the undeclared byte key derives from the declared file count (#6148)"
        );
        let both = Budget::for_engagement(&settings(Some(77), Some(4096)));
        assert_eq!(both.max_bytes, 4096, "an explicit byte budget still wins");
    }

    /// An engagement that declares nothing — and one whose declaration is
    /// unusable — falls through to the machine's answer, rather than reading as
    /// a request for a zero-file investigation.
    #[test]
    fn an_engagement_declaring_nothing_matches_the_machine() {
        assert_eq!(
            Budget::for_engagement(&settings(None, None)),
            Budget::from_env()
        );
        assert_eq!(
            Budget::for_engagement(&settings(Some(0), Some(0))),
            Budget::from_env()
        );
    }

    /// #6082: the key that tells trusty-review the ranking IS the sample. It is
    /// written only when the caller says the search leg worked, because a
    /// complexity-only ranking suppressing the path-name heuristics would leave
    /// the investigation with less than it had before.
    #[test]
    fn attributed_only_is_declared_when_the_search_leg_worked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        let priorities = [Priority::bare("src/pay.rs")];
        for (attributed, expected) in [(true, Some(true)), (false, None)] {
            std::fs::write(&path, SAMPLE).expect("write");
            write_into(
                &path,
                Path::new("/w/repos/acme-api"),
                &priorities,
                None,
                attributed,
                &[],
            )
            .expect("records");
            let out = std::fs::read_to_string(&path).expect("read back");
            let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
            assert_eq!(
                parsed["report"].get("attributed_only").map(|v| v.as_bool()),
                expected.map(Some),
                "attributed={attributed}: {out}"
            );
        }
    }

    /// The whole point: the ranking lands on the right repository, in order, and
    /// the OTHER repository is left exactly as it was.
    #[test]
    fn the_ranking_lands_on_the_matching_repository() {
        let out = written(
            SAMPLE,
            "/w/repos/acme-api",
            &["src/pay.rs", "src/auth.rs"],
            &[],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("still valid TOML");
        let repos = parsed["repositories"].as_array().expect("array");
        assert_eq!(
            repos[0]["inspect_priority"]
                .as_array()
                .expect("declared")
                .iter()
                .map(|v| v.as_str().expect("string"))
                .collect::<Vec<_>>(),
            vec!["src/pay.rs", "src/auth.rs"],
        );
        assert!(
            repos[1].get("inspect_priority").is_none(),
            "the other repository must be untouched: {out}"
        );
    }

    /// The keys `tga` wrote and `trusty-review` reads survive the edit — the
    /// reason this is a `toml_edit` splice rather than a value round trip.
    #[test]
    fn everything_the_manifest_already_said_survives() {
        let out = written(SAMPLE, "/w/repos/acme-api", &["src/pay.rs"], &[]);
        assert!(
            out.contains("title = \"Acme — Technical Due Diligence\""),
            "{out}"
        );
        assert!(out.contains("name = \"02-acme-web\""), "{out}");
        assert!(
            out.contains("Two repositories could not be cloned."),
            "{out}"
        );
    }

    /// A degraded leg has to reach the report. It is appended, and a re-run over
    /// the same manifest does not say it twice.
    #[test]
    fn gaps_are_appended_without_duplicating() {
        let once = written(SAMPLE, "/w/repos/acme-api", &[], &["acme-api: no daemon"]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, &once).expect("write");
        write_into(
            &path,
            Path::new("/w/repos/acme-api"),
            &[],
            None,
            false,
            &["acme-api: no daemon".to_owned()],
        )
        .expect("records");
        let twice = std::fs::read_to_string(&path).expect("read back");
        let parsed: toml::Value = toml::from_str(&twice).expect("valid TOML");
        let gaps = parsed["report"]["gaps"].as_array().expect("array");
        assert_eq!(gaps.len(), 2, "{twice}");
        assert_eq!(
            gaps.iter()
                .filter(|g| g.as_str() == Some("acme-api: no daemon"))
                .count(),
            1,
            "{twice}"
        );
    }

    /// A manifest with no `gaps` key gets one rather than losing the line.
    #[test]
    fn a_manifest_with_no_gaps_key_gains_one() {
        let out = written(
            "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"a\"\npath = \"/w/a\"\n",
            "/w/a",
            &[],
            &["a: no daemon"],
        );
        let parsed: toml::Value = toml::from_str(&out).expect("valid TOML");
        assert_eq!(
            parsed["report"]["gaps"].as_array().expect("array")[0]
                .as_str()
                .expect("string"),
            "a: no daemon"
        );
    }

    /// Nothing to record must not rewrite a file two other crates own.
    #[test]
    fn nothing_to_record_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, SAMPLE).expect("write");
        write_into(&path, Path::new("/w/repos/acme-api"), &[], None, false, &[]).expect("no-op");
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), SAMPLE);
    }

    /// A ranking that cannot be attributed is refused rather than attached to
    /// whichever entry happened to be first — a report claiming trusty-analyze
    /// ranked a repository it never measured is worse than no ranking.
    #[test]
    fn a_ranking_with_no_matching_entry_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, SAMPLE).expect("write");
        let err = write_into(
            &path,
            Path::new("/w/repos/nowhere"),
            &[Priority::bare("src/a.rs")],
            None,
            false,
            &[],
        )
        .expect_err("an unattributable ranking must be refused");
        assert!(err.contains("/w/repos/nowhere"), "{err}");
    }

    #[test]
    fn an_absent_manifest_is_a_reason_not_a_panic() {
        let err = write_into(
            Path::new("/nonexistent/manifest.toml"),
            Path::new("/w/a"),
            &[Priority::bare("src/a.rs")],
            None,
            false,
            &[],
        )
        .expect_err("an absent manifest must degrade");
        assert!(err.contains("could not be read"), "{err}");
    }

    #[test]
    fn a_manifest_that_is_not_toml_is_a_reason_not_a_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("manifest.toml");
        std::fs::write(&path, "this is not toml = = =").expect("write");
        let err = write_into(
            &path,
            Path::new("/w/a"),
            &[],
            None,
            false,
            &["a: no daemon".to_owned()],
        )
        .expect_err("a malformed manifest must degrade");
        assert!(err.contains("not readable as TOML"), "{err}");
    }
}
