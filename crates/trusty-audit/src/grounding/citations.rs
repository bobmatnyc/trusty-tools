//! Checking a finding's citation while the checkout is still on disk (#6791).
//!
//! Why: the trace pass that decides whether a finding's citation still resolves
//! runs at SYNTHESIS — `trusty-review`'s verify stage (#6166). This crate runs
//! synthesis twice: once inside the `tga audit` child, and again whenever the
//! delivered bundle is re-rendered (`crate::rerender`). The second run reads the
//! manifests the package carries and nothing else, because a package
//! deliberately ships no git checkouts (DOC-68 §13). Every citation therefore
//! resolves against a repository that is not there, and the 0.13.2 bundle
//! shipped 661 of 661 findings `unverifiable` for exactly that reason — a render
//! whose every finding reads as unchecked, in a document whose value is that its
//! findings were checked.
//!
//! Collection is the one moment the checkout exists. This leg spends it: it
//! reads the findings `trusty-review` already wrote beside the manifest, checks
//! each one's cited path, line and symbol against the files on disk, and records
//! CONFIRMED / STALE / UNREACHABLE per citation IN THE MANIFEST — the interface
//! that ships with the bundle (owner ruling 2026-08-19). A later render consumes
//! those verdicts rather than producing verdicts it has no repository to produce
//! them from.
//!
//! What: [`ground_into`], the leg [`super::ground_manifest`] calls, over
//! [`read_citations`] → [`judge`] → [`write_into`].
//!
//! ## What a verdict claims, and what it does not
//!
//! It claims exactly what a filesystem read can settle: the cited file is in the
//! checkout, the cited line is inside it, and the traced symbol is still on the
//! line the trace anchored it to. It does NOT re-judge whether the code supports
//! the finding — that is the verifier model's call and it stays where #6166 put
//! it. A CONFIRMED citation is a citation a reader can follow, not a finding a
//! second model agreed with.
//!
//! The evidence quote is deliberately not re-matched here. `trusty-review` owns
//! that matcher (`investigate::verify::find_evidence_match`, whitespace-
//! insensitive over the whole file) and this crate does not depend on that crate
//! — reimplementing it would be the second implementation CLAUDE.md's
//! common-entry-point rule makes a defect, and a divergent copy would report
//! staleness the renderer disagrees with.
//!
//! Test: `super::citations_tests`.

use std::path::{Component, Path};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};

/// The collector's name, at the head of every gap line it writes.
pub const COLLECTOR: &str = "citation-check";

/// The per-repository manifest key these verdicts are written under.
///
/// Why: named on the repository entry rather than in `[report]` because a
/// verdict is about one checkout — the same placement `crate_topology` takes,
/// and for the same reason. Two repositories in one engagement each answer for
/// their own citations.
pub const MANIFEST_KEY: &str = "citation_verdicts";

/// What a collection-time read of the checkout concluded about one citation.
///
/// Why: three outcomes, and the third is not a variety of the second. A citation
/// nobody could check (no checkout) and a citation that was checked and no
/// longer resolves (STALE) are opposite facts, and collapsing them is how a
/// bundle ends up reporting that every finding failed verification when in truth
/// none was ever verified.
/// What: [`Verdict::as_str`] is the manifest spelling and the only one that
/// travels — a renderer reads these strings, never this enum.
/// Test: `super::citations_tests::a_present_citation_is_confirmed`,
/// `…::a_missing_checkout_is_unreachable_not_stale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The cited file, line and symbol are all still there.
    Confirmed,
    /// The checkout was read and does not support the citation.
    Stale,
    /// There was no checkout to read, so nothing was checked.
    Unreachable,
}

impl Verdict {
    /// The token written into the manifest.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Confirmed => "confirmed",
            Verdict::Stale => "stale",
            Verdict::Unreachable => "unreachable",
        }
    }
}

/// One finding's citation, as `trusty-review` recorded it.
///
/// Why: the identity is `(file, title)` because that is the identity every other
/// pass in that crate keys a finding on — `batch::merge_dedupe`,
/// `merge_investigation_prose`, and #6166's own verdict fold all use it. A
/// fourth notion of finding identity here would not join.
/// What: the cited path and line, plus the symbol #6166's trace pass anchored
/// and the line it anchored it at. Both symbol fields are `None` for a finding
/// the trace pass never reached, which leaves the check to the path and line.
/// Test: `super::citations_tests::the_snapshot_yields_one_citation_per_cited_finding`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Citation {
    /// The finding's title.
    pub title: String,
    /// Repository-relative cited path.
    pub file: String,
    /// 1-based cited line, when the finding carried one.
    pub line: Option<u64>,
    /// The traced symbol, as the symbol graph spells it.
    pub symbol: Option<String>,
    /// The line the trace anchored [`Self::symbol`] at.
    pub symbol_line: Option<u64>,
}

/// One citation, and what the checkout said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CitationVerdict {
    /// The citation this verdict is about.
    pub citation: Citation,
    /// The verdict.
    pub verdict: Verdict,
    /// One line saying why, empty for [`Verdict::Confirmed`].
    pub reason: String,
}

/// Every cited finding in one repository's investigation snapshot.
///
/// Why: the snapshot is the only record of what the findings CITE. The manifest
/// carries the assurance-scan rows, which cite a package rather than a line, and
/// the rendered Markdown is prose. Reading the snapshot is how this leg learns
/// what to check, and it is already this crate's channel for that file
/// (`super::osv::inventory` reads its dependency inventory the same way).
/// What: one [`Citation`] per finding that cites a file, joined to its trace
/// anchor by `(file, title)`. A GREEN topic with no citation contributes
/// nothing — there is no path to check.
///
/// # Errors
/// One line, safe to show the recipient, when the snapshot is absent, unreadable
/// or not an investigation snapshot. The caller turns it into a gap.
///
/// # Postconditions
/// On `Ok`, every returned citation has a non-empty `file`.
///
/// Test: `super::citations_tests::{the_snapshot_yields_one_citation_per_cited_finding,
/// a_missing_snapshot_is_a_named_gap, an_anchor_supplies_the_symbol_and_its_line}`.
pub fn read_citations(snapshot: &Path) -> Result<Vec<Citation>, String> {
    let text = std::fs::read_to_string(snapshot).map_err(|e| {
        format!(
            "{COLLECTOR}: the investigation snapshot at {} could not be read ({e})",
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

    let mut citations = Vec::new();
    for repo in repos {
        let anchors = anchors_of(repo);
        for finding in repo
            .get("findings")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let file = str_at(finding, "file").unwrap_or_default();
            if file.is_empty() {
                continue;
            }
            let title = str_at(finding, "title").unwrap_or_default();
            let (symbol, symbol_line) = anchors
                .iter()
                .find(|(f, t, _, _)| *f == file && *t == title)
                .map_or((None, None), |(_, _, s, l)| (Some(s.clone()), Some(*l)));
            citations.push(Citation {
                title,
                file,
                line: finding.get("line").and_then(serde_json::Value::as_u64),
                symbol,
                symbol_line,
            });
        }
    }
    Ok(citations)
}

/// The `(file, title, symbol, line)` of every trace record that reached an
/// anchor, for one repository's snapshot entry.
///
/// A record with no anchor — #6166's fail-closed `no trace:` case — contributes
/// nothing, so the citation it belongs to is checked on its path and line alone
/// rather than against a symbol nobody resolved.
fn anchors_of(repo: &serde_json::Value) -> Vec<(String, String, String, u64)> {
    repo.get("traces")
        .and_then(|t| t.get("traces"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|trace| {
            let anchor = trace.get("anchor")?;
            Some((
                str_at(trace, "file")?,
                str_at(trace, "title")?,
                str_at(anchor, "symbol")?,
                anchor.get("line").and_then(serde_json::Value::as_u64)?,
            ))
        })
        .collect()
}

/// A trimmed string field, `None` when the key is absent or not a string.
fn str_at(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(|s| s.trim().to_string())
}

/// Judge every citation against `checkout`.
///
/// Why: the whole point of the leg, and the one function that touches the
/// repository. The checkout is tested ONCE rather than per citation, because
/// "there is no checkout" is one fact about the run and reporting it 661 times
/// as 661 independent failures is what this leg exists to stop.
///
/// # Postconditions
/// Exactly one verdict per input citation, in input order. Never panics, never
/// reads outside `checkout`, and returns [`Verdict::Unreachable`] for every
/// citation when `checkout` is not a directory.
///
/// Test: `super::citations_tests` drives one test per arm.
#[must_use]
pub fn judge(checkout: &Path, citations: &[Citation]) -> Vec<CitationVerdict> {
    let reachable = checkout.is_dir();
    citations
        .iter()
        .map(|citation| {
            if reachable {
                judge_one(checkout, citation)
            } else {
                CitationVerdict {
                    citation: citation.clone(),
                    verdict: Verdict::Unreachable,
                    reason: format!(
                        "the checkout at {} is not present, so no citation could be checked",
                        checkout.display()
                    ),
                }
            }
        })
        .collect()
}

/// Judge one citation against a checkout known to be present.
fn judge_one(checkout: &Path, citation: &Citation) -> CitationVerdict {
    let stale = |reason: String| CitationVerdict {
        citation: citation.clone(),
        verdict: Verdict::Stale,
        reason,
    };
    let Some(relative) = inside_checkout(&citation.file) else {
        return stale(format!(
            "the cited path `{}` is not inside the checkout",
            citation.file
        ));
    };
    let source = match std::fs::read_to_string(checkout.join(relative)) {
        Ok(source) => source,
        Err(e) => {
            return stale(format!(
                "the cited path `{}` is no longer readable in the checkout ({e})",
                citation.file
            ));
        }
    };
    let lines: Vec<&str> = source.lines().collect();
    if let Some(line) = citation.line
        && line_text(&lines, line).is_none()
    {
        return stale(format!(
            "the cited line {line} is past the end of `{}`, which now has {} line(s)",
            citation.file,
            lines.len()
        ));
    }
    if let (Some(symbol), Some(line)) = (&citation.symbol, citation.symbol_line) {
        let short = short_name(symbol);
        match line_text(&lines, line) {
            Some(text) if text.contains(short) => {}
            _ => {
                return stale(format!(
                    "the traced symbol `{symbol}` is no longer declared at `{}`:{line}",
                    citation.file
                ));
            }
        }
    }
    CitationVerdict {
        citation: citation.clone(),
        verdict: Verdict::Confirmed,
        reason: String::new(),
    }
}

/// The 1-based line's text, `None` when the file has no such line.
fn line_text<'a>(lines: &[&'a str], line: u64) -> Option<&'a str> {
    let index = usize::try_from(line.checked_sub(1)?).ok()?;
    lines.get(index).copied()
}

/// The symbol's own name, without the type or module path that qualifies it.
///
/// The trace pass spells a method `Type::method`, and only the last segment is
/// on the declaration line.
fn short_name(symbol: &str) -> &str {
    symbol.rsplit("::").next().unwrap_or(symbol)
}

/// The citation's path as a checkout-relative path, or `None` when it escapes.
///
/// Why: a cited path reaches this leg from a model-authored finding, so it is
/// untrusted input to a filesystem read. An absolute path or a `..` component
/// would let a citation address a file outside the repository, and a CONFIRMED
/// verdict on such a read would state that the checkout carries a file that is
/// not in it.
/// What: `None` for an absolute path, a root or prefix component, or any `..`;
/// the path itself otherwise, with `.` components left for `join` to absorb.
/// Test: `super::citations_tests::a_citation_escaping_the_checkout_is_never_confirmed`.
fn inside_checkout(file: &str) -> Option<&Path> {
    let path = Path::new(file);
    let contained = path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    contained.then_some(path)
}

/// Write `verdicts` onto the manifest's repository entry for `checkout`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A verdict this
/// process holds and does not write reaches no renderer — least of all
/// `crate::rerender`, which is the render this leg exists for.
/// What: replaces the entry's [`MANIFEST_KEY`] array wholesale, written
/// format-preserving exactly as [`super::topology::write_into`] writes its
/// graph. Replacing rather than appending is what makes a resumed sweep record
/// one verdict per citation instead of two.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, matched or written back.
///
/// # Postconditions
/// On `Ok`, the repository whose `path` is `checkout` declares exactly these
/// verdicts and nothing else in the document changed. An empty `verdicts` writes
/// nothing and cannot fail.
///
/// Test: `super::citations_tests::{the_verdicts_land_on_the_matching_repository,
/// a_second_run_restates_rather_than_duplicates}`.
pub fn write_into(
    manifest: &Path,
    checkout: &Path,
    verdicts: &[CitationVerdict],
) -> Result<(), String> {
    if verdicts.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("{} could not be read ({e})", manifest.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", manifest.display()))?;

    let repositories = doc
        .get_mut("repositories")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "the manifest declares no `[[repositories]]` entry".to_string())?;
    let entry = repositories
        .iter_mut()
        .find(|table| super::priority::names_checkout(table, checkout))
        .ok_or_else(|| {
            format!(
                "no `[[repositories]]` entry names the checkout at {}",
                checkout.display()
            )
        })?;
    entry.insert(MANIFEST_KEY, Item::Value(Value::Array(rows(verdicts))));

    std::fs::write(manifest, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", manifest.display()))
}

/// The verdicts as a multi-line TOML array of inline tables.
fn rows(verdicts: &[CitationVerdict]) -> Array {
    let mut array = Array::new();
    for verdict in verdicts {
        let mut row = InlineTable::new();
        row.insert("title", Value::from(verdict.citation.title.as_str()));
        row.insert("file", Value::from(verdict.citation.file.as_str()));
        if let Some(line) = verdict.citation.line {
            row.insert("line", Value::from(i64::try_from(line).unwrap_or(i64::MAX)));
        }
        if let Some(symbol) = &verdict.citation.symbol {
            row.insert("symbol", Value::from(symbol.as_str()));
        }
        row.insert("verdict", Value::from(verdict.verdict.as_str()));
        if !verdict.reason.is_empty() {
            row.insert("reason", Value::from(verdict.reason.as_str()));
        }
        let mut value = Value::InlineTable(row);
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    array
}

/// Read this repository's citations, judge them against its checkout, and write
/// the verdicts into its manifest.
///
/// Why/What: see the module docs. `snapshot` is the `investigation.json`
/// `trusty-review` wrote beside the manifest ([`super::INVESTIGATION_SNAPSHOT`]).
///
/// # Postconditions
/// Never panics and never returns an error. Every degradation is one line in the
/// returned gap list, naming `display` — the shape [`super::ground_manifest`]
/// gives every leg. A snapshot that does not exist yet is NOT a gap: on a run
/// whose render happens later there is nothing to check and nothing has been
/// lost, which is a different state from a snapshot that exists and cannot be
/// read.
///
/// Test: `super::citations_tests::{a_render_that_has_not_happened_yet_is_silent,
/// stale_citations_are_counted_in_a_gap_line}`.
pub fn ground_into(manifest: &Path, checkout: &Path, display: &str) -> Vec<String> {
    let Some(dir) = manifest.parent() else {
        return vec![format!(
            "{display}: {COLLECTOR}: {} has no parent directory, so this repository's findings \
             could not be located",
            manifest.display()
        )];
    };
    let snapshot = dir.join(super::INVESTIGATION_SNAPSHOT);
    if !snapshot.is_file() {
        return Vec::new();
    }
    let citations = match read_citations(&snapshot) {
        Ok(citations) => citations,
        Err(cause) => {
            return vec![format!(
                "{display}: {cause} — no citation was checked against the checkout, so a later \
                 render of this bundle cannot tell a live citation from one the code has moved \
                 past"
            )];
        }
    };
    if citations.is_empty() {
        return Vec::new();
    }

    let verdicts = judge(checkout, &citations);
    let mut gaps = outcome_gaps(&verdicts, display);
    if let Err(cause) = write_into(manifest, checkout, &verdicts) {
        gaps.push(format!(
            "{display}: {COLLECTOR}: {cause} — the delivered bundle records no citation verdict \
             for this repository, so a later render reports every one of its {} finding(s) as \
             unverified",
            verdicts.len()
        ));
    }
    gaps
}

/// The lines the judged set owes the report.
///
/// A clean pass says nothing: every verdict is in the manifest, which is where a
/// reader checks them. Only the two degradations earn a line — citations that no
/// longer resolve, and a checkout that was not there to ask.
fn outcome_gaps(verdicts: &[CitationVerdict], display: &str) -> Vec<String> {
    let count = |wanted: Verdict| verdicts.iter().filter(|v| v.verdict == wanted).count();
    let mut gaps = Vec::new();
    let unreachable = count(Verdict::Unreachable);
    if unreachable > 0 {
        gaps.push(format!(
            "{display}: {COLLECTOR}: its checkout was not on disk when its {unreachable} \
             citation(s) were checked, so they are recorded unreachable — unassessed, not clean"
        ));
    }
    let stale = count(Verdict::Stale);
    if stale > 0 {
        gaps.push(format!(
            "{display}: {COLLECTOR}: {stale} of {} finding citation(s) no longer resolve against \
             the checkout they were collected from",
            verdicts.len()
        ));
    }
    gaps
}

#[cfg(test)]
#[path = "citations_tests.rs"]
mod citations_tests;
