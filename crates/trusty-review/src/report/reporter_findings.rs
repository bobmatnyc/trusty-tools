//! The merged RED/AMBER finding row — deterministic metrics fields plus the
//! verified-synthesis prose overlaid onto them.
//!
//! Why: split out of `reporter.rs` when #5454's synthesis-required guard pushed
//! that file to 502 SLOC, one over the production cap. This block is the natural
//! seam: `FindingRow` and [`push_finding_band`] are one unit — a struct that
//! exists solely so the band builder can merge two sources by field before
//! converting to a `Scope` once — and nothing else in `reporter.rs` touches
//! either. It joins `reporter_fill` and `reporter_graph_datasets` as a
//! `reporter_*` sibling rather than inventing a new file layout.
//! What: [`push_finding_band`], the only item `reporter.rs` calls, and the
//! private `FindingRow` it builds.
//! Test: `reporter_tests.rs::{reporter_fills_red_findings_deterministically,
//! reporter_merges_synthesis_prose_onto_deterministic_finding,
//! reporter_injects_synthesis_prose,
//! reporter_leaves_findings_honesty_marked_without_metrics}`.

use super::fill::Scope;
use super::metrics::Severity;
use super::model::RepositoryReport;
use super::provenance::{Provenance, tag};

/// One merged RED/AMBER finding row: deterministic metrics fields plus an
/// optional verified-synthesis prose overlay (live-QA defect #2314).
///
/// Why: a plain struct (rather than building [`Scope`]s directly) lets
/// [`push_finding_band`] merge synthesis prose onto an already-built
/// metrics-derived row by field, then convert to a `Scope` once, in one place.
/// What: `title` is always set (metrics or synthesis); every other field is
/// `Option` so an unset one falls through to the fill engine's honesty marker.
/// `evidence_quote` is kept RAW (untagged, un-deduped) separately from
/// `evidence_measured` — findings-rendering defect #2 (#2357 wave-3.2) — so
/// [`into_scope`](FindingRow::into_scope) can compose a fenced code block
/// (label line + verbatim quote) instead of splicing the quote inline into a
/// prose sentence, which mangled markdown and spliced a spurious "No data"
/// line into multi-line quotes (defects #2/#3).
#[derive(Default, Clone)]
struct FindingRow {
    title: String,
    /// True once this row's narrative is settled — a synthesis row (which is
    /// its own narrative) or a metrics row one has merged onto. An unclaimed
    /// row is the only kind [`match_row`] will pair a narrative with, which is
    /// what stops a second narrative sharing the title from overwriting the
    /// first (#6082 lap 5).
    claimed: bool,
    category: Option<String>,
    component: Option<String>,
    description: Option<String>,
    evidence_quote: Option<String>,
    evidence_measured: bool,
    /// The one-line trace verdict marker (#6166 leg 2), or empty. Rendered as an
    /// extra bullet under the evidence block; it is a mechanical record of what
    /// the verifier decided, so it carries no `inferred` tag and is never
    /// punctuation-deduped.
    trace_verdict: String,
    business_impact: Option<String>,
    remediation: Option<String>,
    cost_effort: Option<String>,
}

impl FindingRow {
    /// A bare deterministic row from one `metrics.findings` entry.
    ///
    /// Why: title/category/component are verbatim, always-available facts from
    /// trusty-analyze, and so — since #5317 — are the tool's own observation and
    /// suggested action. Those two were previously left unset and rendered as
    /// the honesty marker even though the daemon had returned them, which made
    /// every analyze-derived entry read as contentless. They are deterministic
    /// tool output, not synthesis, and [`FindingRow::merge_prose`] still
    /// overwrites them when synthesis runs. Business impact and cost/effort stay
    /// unset: nothing in the metrics states either.
    /// What: copies `title`/`category`/`component`/`description`/`remediation`,
    /// mapping an empty source string to `None`.
    /// Test: `reporter_tests.rs::reporter_renders_metric_description_and_remediation`.
    fn from_metric(f: &super::metrics::MetricFinding) -> Self {
        FindingRow {
            title: f.title.clone(),
            category: non_empty(&f.category),
            component: dedupe_field(&f.component),
            description: dedupe_field(&f.description),
            remediation: dedupe_field(&f.remediation),
            ..Default::default()
        }
    }

    /// A synthesis-only row for a verified finding with no `metrics.findings`
    /// title match (e.g. metrics were absent or reported the finding
    /// differently) — preserves synthesis's own title/component alongside its
    /// prose rather than silently dropping verified data.
    ///
    /// Why: the narrative fields are LLM-written, so each is tagged `inferred`
    /// (live-QA wave-2 defect #1); `component` is identifier-like routing data
    /// copied from context rather than a narrative sentence, so it is left
    /// untagged (dedupe-only, no provenance marker).  `evidence_quote` is kept
    /// RAW — never deduped/tagged — because it must render byte-for-byte
    /// verbatim inside a fence (#2357 wave-3.2 defect #2); trimming it would
    /// make the DISPLAYED quote diverge from the quote that was verified.
    /// What: builds the row from `f`, applying [`prose_field`] (dedupe trailing
    /// terminal punctuation + tag `Inferred`) to description/business_impact/
    /// remediation/cost_effort.
    fn from_prose(f: &super::synthesize::FindingProse) -> Self {
        FindingRow {
            title: f.title.clone(),
            claimed: true,
            category: None,
            component: dedupe_field(&f.component),
            description: prose_field(&f.description),
            evidence_quote: raw_evidence(&f.evidence),
            evidence_measured: f.evidence_measured,
            trace_verdict: f.trace_verdict.clone(),
            business_impact: prose_field(&f.business_impact),
            remediation: prose_field(&f.remediation),
            cost_effort: prose_field(&f.cost_effort),
        }
    }

    /// Overlay verified synthesis prose onto this already metrics-backed row.
    ///
    /// Why: this is the "layer prose on top" step — the row already exists
    /// (from metrics), so synthesis fills the gaps rather than creating a
    /// second, duplicate entry for the same finding.  Each narrative field is
    /// tagged `inferred` (live-QA wave-2 defect #1).
    /// What: sets description/evidence_quote/business_impact/remediation/
    /// cost_effort from `f`; `component` is kept metrics-verbatim unless
    /// metrics left it unset, in which case synthesis's value is used as a
    /// dedupe-only (untagged) fallback.
    fn merge_prose(&mut self, f: &super::synthesize::FindingProse) {
        self.description = prose_field(&f.description);
        self.evidence_quote = raw_evidence(&f.evidence);
        self.evidence_measured = f.evidence_measured;
        self.trace_verdict = f.trace_verdict.clone();
        self.business_impact = prose_field(&f.business_impact);
        self.remediation = prose_field(&f.remediation);
        self.cost_effort = prose_field(&f.cost_effort);
        if self.component.is_none() {
            self.component = dedupe_field(&f.component);
        }
    }

    /// Convert this row into a fill [`Scope`] for one `finding_block`
    /// repetition, numbered `index` within its severity section (#2357
    /// wave-3.2 defect #1 — a real sequential number, never a literal `N.`).
    fn into_scope(self, index: usize) -> Scope {
        let mut s = Scope::new();
        s.set("finding_index", index.to_string());
        s.set("finding_title", self.title);
        s.set_opt("finding_category", self.category);
        s.set_opt("finding_description", self.description);
        s.set_opt("finding_business_impact", self.business_impact);
        s.set_opt("finding_remediation", self.remediation);
        s.set_opt("finding_cost_effort", self.cost_effort);
        if let Some(quote) = &self.evidence_quote {
            s.set(
                "finding_evidence_block",
                render_evidence_block(
                    self.component.as_deref().unwrap_or(""),
                    quote,
                    self.evidence_measured,
                    &self.trace_verdict,
                ),
            );
        }
        s.set_opt("finding_component", self.component);
        s
    }
}

/// Is this row a restatement of itself rather than a finding? (#6082)
///
/// Why: three of one report's 157 findings were the model re-reporting findings
/// it had already made. #156 "Extract method — hnsw_store" and #157 "Extract
/// method — Jira backend" duplicated analyze findings #1 and #3, and #2
/// duplicated the tickets-server one. All three share a shape the legitimate
/// findings never have: the Evidence block quotes the finding's own LABEL —
/// `crates/…/hnsw_store.rs (extract method)` — instead of a line of source, and
/// the Component is a prose phrase (`trusty-common memory_core`) instead of a
/// path. A real finding quotes code and cites a file; these quote themselves and
/// cite a topic, which is what makes them mechanically identifiable.
/// What: true when a quoted row's component names no file, when the quote NAMES
/// its own file instead of quoting a line out of it, or when the quote and the
/// title restate each other. All three compare on alphanumerics only, so
/// punctuation, case and spacing cannot hide a restatement. A row with no
/// evidence quote at all is NOT suppressed — that is an ordinary un-synthesized
/// metrics row, which has never claimed to carry evidence.
/// Test: `reporter_tests::{a_self_quoting_finding_is_suppressed,
/// a_finding_with_a_non_path_component_is_suppressed,
/// a_genuine_finding_survives_the_self_quote_filter,
/// a_root_level_manifest_component_is_not_a_topic}`.
fn is_self_restatement(row: &FindingRow) -> bool {
    let Some(quote) = row.evidence_quote.as_deref() else {
        return false;
    };
    let component = row.component.as_deref().unwrap_or_default();
    if !names_a_file(component) {
        return true;
    }
    let q = alphanumeric(quote);
    if q.is_empty() {
        return false;
    }
    // A finding's evidence is a line OUT OF the cited file. A quote that spells
    // the file's own path is naming the finding, not evidencing it — #2 of the
    // graded report quoted `crates/…/tickets/server.rs (extract method —
    // dispatch)` against that very file.
    let path = alphanumeric(component.rsplit_once(':').map_or(component, |(p, _)| p));
    if !path.is_empty() && q.contains(&path) {
        return true;
    }
    let title = alphanumeric(&row.title);
    !title.is_empty() && (q.contains(&title) || title.contains(&q))
}

/// The row a synthesis narrative belongs to, when that pairing is unambiguous.
///
/// Why: matching on title text alone cross-wired two findings and lost a third
/// (#6082 lap 5). The graded report carried 15 metrics rows titled "Split
/// oversized impl block"; both synthesis narratives that shared that title
/// matched the FIRST of them, so the `hnsw_store.rs` row rendered the Jira
/// backend's narrative, the hnsw narrative was never rendered at all, and the
/// Jira row never received its own. Title is an ambiguous key whenever the
/// analyze daemon emits one canned remediation title per hotspot, which is its
/// normal output — so the component has to break the tie.
/// What: candidates are the UNCLAIMED rows carrying the same title, which alone
/// stops a second narrative from overwriting the first. A single candidate is
/// matched on title as before, so the common unique-title case is unchanged.
/// Two or more are disambiguated by component identity — the narrative's own
/// `component` when the model wrote a path, else its `evidence` line, which
/// names the file even when `component` is a topic phrase. Exact matches are
/// preferred over suffix matches, and an unresolvable tie matches nothing
/// rather than guessing.
/// Test: `reporter_tests::{colliding_titles_attach_to_their_own_components,
/// a_second_narrative_cannot_overwrite_a_claimed_row}`.
fn match_row(rows: &[FindingRow], f: &super::synthesize::FindingProse) -> Option<usize> {
    let candidates: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.claimed && r.title == f.title)
        .map(|(i, _)| i)
        .collect();
    match candidates.as_slice() {
        [] => None,
        [only] => Some(*only),
        many => {
            let ids: Vec<String> = [
                f.component.as_str(),
                f.evidence.lines().next().unwrap_or(""),
            ]
            .iter()
            .map(|c| normalized_path(c))
            .filter(|c| !c.is_empty())
            .collect();
            let row_path = |i: &usize| normalized_path(rows[*i].component.as_deref().unwrap_or(""));
            many.iter()
                .find(|i| {
                    let p = row_path(i);
                    !p.is_empty() && ids.contains(&p)
                })
                .or_else(|| {
                    many.iter().find(|i| {
                        let p = row_path(i);
                        !p.is_empty() && ids.iter().any(|c| c.ends_with(&p) || p.ends_with(c))
                    })
                })
                .copied()
        }
    }
}

/// A component reduced to a comparable file identity: the path half of a
/// `path:line` citation, lower-cased down to its alphanumerics so punctuation
/// and separator differences cannot hide a match.
fn normalized_path(component: &str) -> String {
    let path = component.rsplit_once(':').map_or(component, |(p, _)| p);
    alphanumeric(path.trim())
}

/// Does `component` name a file rather than a topic?
///
/// A path separator or a file extension is enough — `Cargo.toml` and `deny.toml`
/// are real root-level citations with neither a directory nor a line number, and
/// must not be mistaken for prose.
fn names_a_file(component: &str) -> bool {
    let path = component.rsplit_once(':').map_or(component, |(p, _)| p);
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    path.contains('/')
        || path.contains('\\')
        || path
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && ext.chars().all(|c| c.is_alphanumeric()))
}

/// A string reduced to its lower-cased alphanumerics, so a restatement cannot
/// hide behind punctuation, case, or spacing differences.
fn alphanumeric(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Map an empty string to `None` so a blank/absent value falls to the honesty
/// marker instead of rendering as literal empty text.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Strip a single trailing sentence-terminal character (`.`/`?`/`!`) from prose
/// destined for a template slot that appends its own literal period right after
/// the value — avoids a doubled terminal punctuation mark (live-QA wave-2
/// defect #3: `report-technical-dd{,-cast}.md` write `{{finding_description}}.`,
/// `{{finding_evidence}}.`, and (in the AMBER row) `{{finding_remediation}}.`).
///
/// Why: LLM-written prose routinely ends its own sentence with terminal
/// punctuation; the template's trailing literal `.` then collides with it
/// (`"...concatenation.."`).  A trailing `)` is left untouched — no template in
/// this crate appends a bare period directly after a field closed with a
/// parenthesis (the RED row wraps `finding_cost_effort` in `(cost/effort: …).`,
/// not the description/evidence/remediation fields), so there is no collision
/// to resolve for that case, and blindly chopping a legitimate closing paren
/// would corrupt otherwise-correct prose.
/// What: trims trailing whitespace, then removes exactly one trailing `.`, `?`,
/// or `!` if present; a trailing `)` (or anything else) is returned unchanged.
/// Test: `reporter_tests.rs::{dedupe_strips_trailing_period,
/// dedupe_strips_trailing_question_mark, dedupe_leaves_trailing_paren}`.
pub(super) fn dedupe_terminal_punctuation(s: &str) -> &str {
    let trimmed = s.trim_end();
    match trimmed.as_bytes().last() {
        Some(b'.') | Some(b'?') | Some(b'!') => &trimmed[..trimmed.len() - 1],
        _ => trimmed,
    }
}

/// Map an empty string (after terminal-punctuation dedupe) to `None`, with no
/// provenance tag — for identifier-like fields (e.g. `component`) that share the
/// same template punctuation collision as narrative prose but are not
/// themselves LLM-authored sentences.
fn dedupe_field(s: &str) -> Option<String> {
    let cleaned = dedupe_terminal_punctuation(s);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Map an empty string (after terminal-punctuation dedupe) to `None`, tagging a
/// non-empty result `inferred` — for the LLM-authored narrative fields of a
/// synthesized finding (live-QA wave-2 defect #1: the marker existed but was
/// never applied to any synthesized content).
fn prose_field(s: &str) -> Option<String> {
    dedupe_field(s).map(|v| tag(v, Provenance::Inferred))
}

/// Trim an evidence quote and map empty to `None` — deliberately NO dedupe of
/// trailing punctuation and NO provenance tag baked into the string (#2357
/// wave-3.2 defect #2): the quote must render byte-for-byte verbatim inside a
/// fenced code block, so nothing here may alter a single character of it.
/// Test: `reporter_tests::evidence_renders_as_fenced_block`.
fn raw_evidence(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Render a finding's evidence as a labeled fenced code block.
///
/// Why: dumping a verbatim multi-line code snippet inline into a prose
/// sentence mangled rendering (unescaped `<`, `{`, `_`, backticks) and a blank
/// line inside the unfenced quote read as a section boundary to the
/// omit-empty polish pass, splicing a spurious "No data available" line mid-
/// quote (#2357 wave-3.2 defects #2/#3). Fencing the quote makes both
/// structurally impossible: nothing inside a fence needs escaping, blank lines
/// are literal, and `polish`'s fence-tracking guard (added alongside this fix)
/// never touches fenced content.
/// What: `file_label` (e.g. `lib/auth/session.ts:58`, or empty) becomes a
/// backtick-wrapped label on its own bulleted line carrying the provenance
/// marker; `quote` is embedded verbatim inside a fence chosen one backtick
/// longer than the longest backtick run the quote itself contains (see
/// [`fence_for`]), so a quote that itself contains a ``` fence still renders
/// correctly.
///
/// #6166 leg 2: a non-empty `trace_verdict` adds one `- **Trace:**` bullet
/// AFTER the fence. It is placed outside the fence because it is a statement
/// about the quote, not part of it — the verified quote must stay byte-for-byte
/// what the guardrail matched. An empty verdict adds nothing, so a finding the
/// verdict pass did not reach renders byte-identically to before.
/// Test: `reporter_tests::{evidence_renders_as_fenced_block,
/// evidence_with_blank_line_fences_cleanly,
/// evidence_containing_triple_backticks_uses_longer_fence,
/// a_trace_verdict_renders_under_the_evidence_fence,
/// no_trace_verdict_leaves_the_evidence_block_unchanged}`.
fn render_evidence_block(
    file_label: &str,
    quote: &str,
    measured: bool,
    trace_verdict: &str,
) -> String {
    let prov_tag = if measured {
        Provenance::Measured
    } else {
        Provenance::Inferred
    }
    .tag();
    let label = if file_label.is_empty() {
        format!("- **Evidence**{prov_tag}:")
    } else {
        format!("- **Evidence** (`{file_label}`){prov_tag}:")
    };
    let fence = fence_for(quote);
    let block = format!("{label}\n{fence}\n{quote}\n{fence}");
    if trace_verdict.trim().is_empty() {
        return block;
    }
    format!("{block}\n- **Trace:** {}", trace_verdict.trim())
}

/// Choose a fence marker at least one backtick longer than the longest run of
/// consecutive backticks in `content`, minimum 3 (a plain ``` fence).
///
/// Why: a fence no longer than a backtick run already inside the content would
/// itself be misparsed as the closing fence, corrupting the render; scanning
/// for the longest run and going one longer is the standard CommonMark-safe
/// technique.
/// Test: `reporter_tests::evidence_containing_triple_backticks_uses_longer_fence`.
fn fence_for(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat((max_run + 1).max(3))
}

/// Fill one severity band's per-application finding blocks for one repository,
/// merging deterministic metrics data with verified synthesis prose.
///
/// Why: M1 must never leave a RED/AMBER finding entirely unlisted just because
/// synthesis is off, unavailable, or guardrail-rejected — title/severity
/// (implied by which block)/category/component are verbatim, always-available
/// facts from `metrics.findings`.  When synthesis IS available, its prose is
/// merged onto the SAME row — never pushed as a second row — so a finding
/// renders exactly once no matter the synthesis outcome.  A metrics row is
/// never deleted (#6082 lap 5): it is the measurement, and the narrative is
/// only an overlay on it.
/// What: builds one [`FindingRow`] per `repo.metrics.findings` entry of `band`
/// (bare fields), then — when `syn` is `Some` — overlays each `FindingProse`
/// onto the row [`match_row`] pairs it with; a narrative with no match is
/// appended as an additional, synthesis-only row so no verified data is
/// silently dropped.  A narrative that merely restates its own finding
/// ([`is_self_restatement`], #6082) is refused: on a metrics row the overlay is
/// skipped and the raw metrics fields render, and a synthesis-only row is
/// dropped outright.  Pushes the `app_block` (`app_name` + one `finding_block`
/// per row) only when the merged list is non-empty.
/// Test: `reporter_tests.rs::{reporter_fills_red_findings_deterministically,
/// reporter_merges_synthesis_prose_onto_deterministic_finding,
/// reporter_injects_synthesis_prose,
/// reporter_leaves_findings_honesty_marked_without_metrics}`.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_finding_band(
    root: &mut Scope,
    repo: &RepositoryReport,
    syn: Option<&super::synthesize::Synthesis>,
    band: Severity,
    band_label: &str,
    app_block: &str,
    finding_block: &str,
    index: &mut usize,
) {
    let mut rows: Vec<FindingRow> = repo
        .metrics
        .iter()
        .flat_map(|m| m.findings.iter())
        .filter(|f| f.severity == band)
        .map(FindingRow::from_metric)
        .collect();

    if let Some(syn) = syn {
        for f in syn
            .findings
            .iter()
            .filter(|f| f.app_slug == repo.slug && f.severity.to_uppercase() == band_label)
        {
            match match_row(&rows, f) {
                // #6082 lap 5: a narrative that merely restates the finding is
                // refused, but the metrics row it would have landed on stays.
                // Dropping the merged row deleted the measurement too — the
                // graded report lost `tickets/server.rs` (cyclomatic 118, grade
                // F) entirely and still exited 0, rendering 167 of 168 AMBER
                // findings. A refused narrative falls back to the raw metrics
                // fields, which is how every un-synthesized hotspot renders.
                Some(i) => {
                    let mut merged = rows[i].clone();
                    merged.merge_prose(f);
                    if !is_self_restatement(&merged) {
                        merged.claimed = true;
                        rows[i] = merged;
                    }
                }
                // A synthesis-only row has no measurement behind it, so a
                // restatement here is dropped: nothing else would render.
                None => {
                    let row = FindingRow::from_prose(f);
                    if !is_self_restatement(&row) {
                        rows.push(row);
                    }
                }
            }
        }
    }

    if rows.is_empty() {
        return;
    }

    let mut app_scope = Scope::new();
    app_scope.set("app_name", repo.name.clone());
    for row in rows {
        *index += 1;
        app_scope.push_block(finding_block, row.into_scope(*index));
    }
    root.push_block(app_block, app_scope);
}
