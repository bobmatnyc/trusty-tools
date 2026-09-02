//! Wire diagnostics and refactor suggestions → deterministic `MetricFinding`s.
//!
//! Why: split out of `analyze_adapter.rs` when the #6082 render fixes pushed
//! that file to 554 SLOC, over the 500 production cap. This is the natural
//! seam — the adapter is transport plus a pure mapping, and everything here is
//! the per-finding half of that mapping: how one wire record becomes one
//! rendered finding, including the two judgements #6082 added (a region with no
//! function name is an impl block, not a function; a component is cited
//! repository-relative). Nothing here touches HTTP or the report model.
//! What: [`diagnostic_finding`], [`refactor_finding`] and
//! [`relativize_components`], with their private helpers.
//! Test: `analyze_adapter_tests.rs`, through the adapter that imports them.

use std::path::Path;

use super::analyze_adapter::{
    WireDiagnostic, WireRefactor, map_diagnostic_severity, map_refactor_severity,
};
use super::metrics::{AnalyzeMetrics, MetricFinding, Severity};

// ─── Finding synthesis (prose-free, deterministic) ───────────────────────────

/// Build a [`MetricFinding`] from a tool diagnostic, or `None` when it maps to
/// the GREEN band (never rendered — omitted to keep the findings list
/// actionable, per the report's no-green rule).
///
/// Why: RED/AMBER findings must be listed deterministically with no LLM prose;
/// `title`/`category`/`component` are verbatim facts.
/// What: `title` = the rule code when present, else a synthesised
/// `"{tool} diagnostic"`; `category` = the producing tool (a stable provenance
/// category, not the prose message); `component` = the file; `severity` via the
/// diagnostic map; `description` = the linter's own message, verbatim (#5317 —
/// dropping it left every rendered prose slot reading the honesty marker).
/// Test: `diagnostic_finding_synthesises_title` and `..._drops_green`.
pub(super) fn diagnostic_finding(d: &WireDiagnostic) -> Option<MetricFinding> {
    let severity = map_diagnostic_severity(&d.severity);
    if severity == Severity::Green {
        return None;
    }
    let title = d.code.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| {
        if d.tool.is_empty() {
            "diagnostic".to_string()
        } else {
            format!("{} diagnostic", d.tool)
        }
    });
    Some(MetricFinding {
        title,
        severity,
        category: if d.tool.is_empty() {
            "diagnostic".to_string()
        } else {
            d.tool.clone()
        },
        component: d.file.clone(),
        description: d.message.clone(),
        remediation: String::new(),
    })
}

/// Build a [`MetricFinding`] from a refactor suggestion, or `None` when it maps
/// to GREEN (omitted, as above).
///
/// Why: high/critical refactor suggestions are actionable maintainability
/// findings that belong in the AMBER band even with no synthesis — never RED,
/// see [`map_refactor_severity`].
/// What: `title` = the humanised refactor type plus the function name when
/// known (e.g. `"Extract method — parse_config"`); `category` =
/// `"maintainability"` (refactors are maintainability by construction);
/// `component` = the file; `severity` via the refactor map; `description` =
/// the analyzer's `rationale` and `remediation` = its `suggested_action`, both
/// verbatim (#5317 — dropping them is what made the entries contentless).
/// A suggestion carrying NO function name is a whole-`impl` region rather than
/// a function, and is relabelled by [`impl_block_finding`] (#6082) — unless the
/// daemon named the region a Python class body, which
/// [`class_body_finding`] relabels instead (#6177).
/// Test: `refactor_finding_synthesises_title`,
/// `refactor_finding_carries_rationale_and_action`,
/// `a_nameless_region_is_labelled_an_impl_block`,
/// `a_python_class_body_is_labelled_a_class_body`.
pub(super) fn refactor_finding(r: &WireRefactor) -> Option<MetricFinding> {
    let severity = map_refactor_severity(&r.severity);
    if severity == Severity::Green {
        return None;
    }
    // #6177: the daemon's own answer about the region outranks the
    // name-absent inference below — a Python class body can carry a name and
    // still not be a function.
    if r.region_kind.as_deref() == Some(CLASS_BODY_REGION) {
        return class_body_finding(r, severity);
    }
    let action = humanise_refactor_type(&r.refactor_type);
    let Some(function) = r.function_name.as_deref().filter(|f| !f.is_empty()) else {
        return impl_block_finding(r, severity);
    };
    Some(MetricFinding {
        title: format!("{action} — {function}"),
        severity,
        category: "maintainability".to_string(),
        component: r.file.clone(),
        description: r.rationale.clone(),
        // The daemon states the placeholder even when it named the function in
        // the same payload; with a name in hand the honest render is the name.
        remediation: r
            .suggested_action
            .replace(NAMELESS_REGION, &format!("`{function}`")),
    })
}

/// The literal the daemon substitutes when it has no name for the region.
///
/// It reaches the page verbatim — "Extract the body of 'this function' (lines
/// 342-945) into 2-3 smaller functions" appeared 17 times in one report — so it
/// is also the marker that identifies a nameless region's prose.
const NAMELESS_REGION: &str = "'this function'";

/// Build the finding for a refactor suggestion with no function name (#6082).
///
/// Why: trusty-analyze reports an `impl X { … }` block as a hotspot region with
/// `function_name: None`, and every downstream string then called it a function.
/// The dogfood report's top four maintainability findings were all `impl`
/// blocks — `impl HnswStore`, `impl Backend for JiraBackend`,
/// `impl AzureDevOpsClient`, `impl ChatSessionStore` — each titled "Extract
/// method", described with a `long_function` smell, and remediated by
/// "Extract the body of 'this function' into 2-3 smaller functions". Extracting
/// the body of an impl block is not an action a reader can take; splitting the
/// impl across submodules is.
/// What: titles the finding an impl block, rewrites the smell vocabulary in the
/// analyzer's rationale, and replaces the remediation with impl-splitting
/// guidance that keeps the analyzer's own line range when it stated one. When
/// the suggestion carries neither a line range nor a rationale there is nothing
/// left that distinguishes the region, and the finding is SUPPRESSED rather
/// than mislabelled.
/// Test: `a_nameless_region_is_labelled_an_impl_block`,
/// `a_nameless_region_never_prints_the_this_function_placeholder`,
/// `an_unidentifiable_nameless_region_is_suppressed`.
fn impl_block_finding(r: &WireRefactor, severity: Severity) -> Option<MetricFinding> {
    let lines = line_range(&r.suggested_action);
    let rationale = r.rationale.trim();
    if lines.is_none() && rationale.is_empty() {
        return None;
    }
    let where_ = lines
        .map(|(a, b)| format!(" (lines {a}–{b})"))
        .unwrap_or_default();
    Some(MetricFinding {
        title: IMPL_BLOCK_TITLE.to_string(),
        severity,
        category: "maintainability".to_string(),
        component: r.file.clone(),
        // `long_function` is the analyzer's smell name for a region it measured
        // as a function; on an impl block it is the same mislabel one level down.
        description: rationale.replace("long_function", "long_impl_block"),
        remediation: format!(
            "{IMPL_BLOCK_REMEDIATION_PREFIX}{where_} across focused submodules — the analyzer \
             reported no function name for this region, so it is a whole-impl hotspot rather than \
             one long function"
        ),
    })
}

/// The `region_kind` value trusty-analyze emits for a Python class body (#6177).
///
/// The wire spelling of `trusty_analyze::core::RegionKind::ClassBody`. The two
/// crates have no Cargo edge on this path — the JSON is the whole seam — so this
/// literal is the contract, exactly as `IMPL_BLOCK_TITLE` is for the renderer
/// below.
const CLASS_BODY_REGION: &str = "class_body";

/// Build the finding for a region the daemon named a Python class body (#6177).
///
/// Why: the Rust half of this already exists — [`impl_block_finding`] catches a
/// region with no function name and says "split the impl block" rather than
/// "extract the body of this function". Python had the same shape and no signal
/// to act on: a `class Foo:` body arrives measured like any other region, and
/// every downstream string called it a function. A lap-4 grader on the
/// trusty-audit self-audit report raised it. Extracting the body of a class is
/// not an action a reader can take; moving its members out is.
/// What: titles the finding a class body, rewrites the analyzer's
/// `long_function` smell name, and replaces the remediation with member-splitting
/// guidance carrying the analyzer's own line range when it stated one. Names the
/// class when the daemon named it. A region with neither a range nor a rationale
/// is SUPPRESSED rather than mislabelled, matching [`impl_block_finding`].
/// Test: `a_python_class_body_is_labelled_a_class_body`,
/// `a_named_python_class_body_is_named_in_its_remediation`,
/// `an_unidentifiable_class_body_is_suppressed`.
fn class_body_finding(r: &WireRefactor, severity: Severity) -> Option<MetricFinding> {
    let lines = line_range(&r.suggested_action);
    let rationale = r.rationale.trim();
    if lines.is_none() && rationale.is_empty() {
        return None;
    }
    let named = r
        .function_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| format!(" `{n}`"))
        .unwrap_or_default();
    let where_ = lines
        .map(|(a, b)| format!(" (lines {a}–{b})"))
        .unwrap_or_default();
    Some(MetricFinding {
        title: CLASS_BODY_TITLE.to_string(),
        severity,
        category: "maintainability".to_string(),
        component: r.file.clone(),
        description: rationale.replace("long_function", "long_class_body"),
        remediation: format!(
            "{CLASS_BODY_REMEDIATION_PREFIX}{named}{where_} by moving cohesive members into \
             collaborating classes or modules — the analyzer measured this region as a class \
             body, so it is not one long function"
        ),
    })
}

/// The title every Python class-body hotspot finding carries (#6177).
pub(crate) const CLASS_BODY_TITLE: &str = "Split oversized class body";

/// The opening words of every class-body remediation, up to the class name and
/// line range when the analyzer stated them (#6177).
pub(crate) const CLASS_BODY_REMEDIATION_PREFIX: &str = "Split the class body";

/// The title every whole-impl hotspot finding carries.
///
/// Shared with [`super::investigate::regions`], which recognises a rendered
/// region finding by this title and re-derives the region's span and score from
/// it (#6082 lap 4). Changing it here without changing that consumer would
/// silently switch the LLM-finding region checks off.
pub(crate) const IMPL_BLOCK_TITLE: &str = "Split oversized impl block";

/// The opening words of every whole-impl hotspot remediation, up to and
/// including the line range when one was stated.
///
/// Shared with [`super::investigate::regions`] for the same reason as
/// [`IMPL_BLOCK_TITLE`].
pub(crate) const IMPL_BLOCK_REMEDIATION_PREFIX: &str = "Split the impl block";

/// The `(first, last)` line range stated in a suggested action, when it states one.
///
/// The daemon writes it as `(lines 342-945)`; the dash is an ASCII hyphen or an
/// en dash depending on version, so both are accepted. `None` when the prose
/// carries no range — which is what makes the finding unidentifiable.
/// Test: `a_nameless_region_is_labelled_an_impl_block`.
fn line_range(action: &str) -> Option<(u64, u64)> {
    let rest = action.split_once("(lines ")?.1;
    let inner = rest.split_once(')')?.0;
    let (a, b) = inner
        .split_once('–')
        .or_else(|| inner.split_once('-'))
        .or_else(|| inner.split_once('—'))?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Convert a snake_case refactor type into a readable title fragment.
///
/// Why: keeps synthesised titles legible without pulling in trusty-analyze's
/// enum. What: `extract_method` → `"Extract method"`; unknown/empty →
/// `"Refactor"`. Test: `refactor_finding_synthesises_title`.
fn humanise_refactor_type(t: &str) -> String {
    if t.is_empty() {
        return "Refactor".to_string();
    }
    let mut words = t.split('_').filter(|w| !w.is_empty());
    let mut out = String::new();
    if let Some(first) = words.next() {
        let mut chars = first.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    for w in words {
        out.push(' ');
        out.push_str(w);
    }
    out
}

// ─── Repo-relative components (#6082) ────────────────────────────────────────

/// Strip the checkout root from every finding's `component`, in place.
///
/// Why (#6082): trusty-analyze indexes a checkout by absolute path and reports
/// findings by absolute path, so the rendered report carried the auditor's own
/// filesystem layout —
/// `/Users/masa/dogfood-audit-rerun4/repos/local/trusty-tools/crates/…` — in 21
/// places, in a document written for someone outside the machine that produced
/// it. The investigation pass already cites repository-relative paths, so the
/// same report used two different path vocabularies for the same files. The
/// local prefix is noise to every reader and an information leak to an external
/// one.
/// What: rewrites each `component` that lies under `root` to its path relative
/// to `root`, preserving any trailing `:line` suffix. A component already
/// relative, or under a different root, is left exactly as it is — this
/// normalises paths, it never rewrites ones it does not recognise.
/// Test: `analyze_adapter_tests::{components_are_made_repo_relative,
/// a_component_outside_the_checkout_is_left_alone}`.
pub(super) fn relativize_components(metrics: &mut AnalyzeMetrics, root: &Path) {
    for finding in &mut metrics.findings {
        if let Some(rel) = repo_relative(&finding.component, root) {
            finding.component = rel;
        }
    }
}

/// `component` expressed relative to `root`, or `None` when it does not lie
/// under it. A trailing `:line` suffix is preserved.
fn repo_relative(component: &str, root: &Path) -> Option<String> {
    let (path, suffix) = match component.rsplit_once(':') {
        Some((p, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            (p, format!(":{line}"))
        }
        _ => (component, String::new()),
    };
    let rel = Path::new(path).strip_prefix(root).ok()?;
    let rel = rel.to_str()?;
    if rel.is_empty() {
        return None;
    }
    Some(format!("{rel}{suffix}"))
}
