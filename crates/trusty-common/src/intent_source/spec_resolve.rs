//! SLD spec-resolution: changed file → governing spec section (C4 hardened).
//!
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)
//!
//! Why: when a ticket is silent, the intent may live in a spec the changed
//! code links to. Per SLD (`spec-linked-docs`), Rust code declares that linkage
//! in **rustdoc** (`# Spec References` blocks), not free comments. The ISR
//! resolves those declared links to a `SpecRef` + extracts the spec's
//! prescribed method from the governed section's **Behavior Contract +
//! Rationale** prose (spec §6.4). **The ISR does not invent linkage** — a file
//! with no SLD ref yields no spec method (a gap on the spec axis).
//! What: `parse_spec_refs` (find `SPEC-…` ids + anchors declared in a changed
//! file's `# Spec References` rustdoc blocks) and `resolve_spec_section`
//! (resolve an anchor — revision-tolerantly — to its governing section, lift
//! the Behavior-Contract + Rationale prose, run the method heuristic, and flag
//! revision drift). `extract_spec_method` is the thin back-compat wrapper the
//! C1 calling convention uses.
//! Test: `super::tests::spec_resolve_*` (AC-6); contributes to AC-18.
//!
//! C4 hardening (#1361) over the C1 minimal reader:
//! - SLD-block scoping: refs are only honoured inside an actual module-level
//!   `//! # Spec References` or fn-level `/// # Spec References` block (§2.4),
//!   so a `SPEC-…` link sitting in unrelated prose is not treated as linkage.
//! - Behavior-Contract + Rationale extraction: the spec method is lifted from
//!   exactly those two labelled sub-blocks of the governed section (§6.4),
//!   not the whole section body.
//! - Revision drift (§6.4, OQ-6): a `~v1` ref pointing at a `~v2` section STILL
//!   resolves (from the current section) and reports `revision_drift = true`,
//!   `stale_spec`-adjacent metadata that callers surface WITHOUT blocking.
//!   OUTDATED enforcement is a NON-GOAL (§1.3) and is not implemented.

use std::sync::OnceLock;

use regex::Regex;

use super::types::{Method, MethodKind, SpecRef};

/// The outcome of resolving one SLD anchor to its governing spec section.
///
/// Why: C4 must report more than "did a method resolve?" — callers need the
/// extracted method AND whether the referenced revision drifted from the
/// section's current revision, so the resolver can flag `stale_spec`-adjacent
/// metadata without blocking (spec §6.4, OQ-6). Bundling both keeps the drift
/// signal attached to the resolution that produced it.
/// What: the `Method` extracted from the section's Behavior-Contract + Rationale
/// prose (`None` when the section prescribes none), the section's own revision
/// as found in its heading anchor, and `revision_drift` (true when the
/// referencing anchor's revision differs from the section's).
/// Test: `super::tests::spec_resolve_drift_*`, `spec_resolve_method_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecSectionResolution {
    /// The method lifted from the section's Behavior-Contract + Rationale.
    pub method: Option<Method>,
    /// The revision tag found on the resolved section heading (`draft`, `v2`).
    pub section_revision: Option<String>,
    /// True when the referencing anchor's revision differs from the section's
    /// (e.g. code references `~v1`, the section is `~v2`). Non-blocking
    /// `stale_spec`-adjacent metadata; OUTDATED enforcement is out of scope.
    pub revision_drift: bool,
}

/// Parse SLD spec references out of a changed file's source text.
///
/// Why: the ISR must find the `SPEC-{SUBSYSTEM}-{NN}~v{rev}` ids a file
/// declares — and only those declared inside an SLD `# Spec References` rustdoc
/// block (module-level `//!` or function-level `///`, per §2.4), not a bare
/// `SPEC-…` link sitting in unrelated prose (the ISR never invents linkage,
/// spec §6.4).
/// What: scans the source for `# Spec References` heading lines (in `//!`/`///`
/// rustdoc), then collects the SLD link form
/// ``[`SPEC-X-NN~vR`](docs/specs/file.md#SPEC-X-NN~vR)`` appearing within that
/// block (until the rustdoc run ends or a new rustdoc heading begins). Returns
/// one `SpecRef` per distinct id, in first-seen order, or an empty vec when the
/// file declares no SLD reference.
/// Test: `super::tests::spec_resolve_parse_*` (AC-6).
#[must_use]
pub fn parse_spec_refs(source: &str) -> Vec<SpecRef> {
    let mut refs: Vec<SpecRef> = Vec::new();
    for block in spec_reference_blocks(source) {
        for caps in sld_link_re().captures_iter(&block) {
            let spec_id = caps[1].to_string();
            let file = caps[2].to_string();
            let anchor = caps[3].to_string();
            // Defence in depth (the canonicalization guard in `FsSpecLookup::load`
            // is the primary control): reject any captured path containing a `..`
            // traversal segment so a malicious source file cannot point linkage
            // outside `docs/specs/`. The `regex` crate has no look-around, so this
            // is a post-match filter rather than a pattern exclusion.
            if file.split('/').any(|seg| seg == "..") {
                continue;
            }
            // De-duplicate on the spec id (first-seen wins) to keep the result
            // deterministic when the same ref appears in both module- and
            // function-level rustdoc.
            if refs.iter().any(|r| r.spec_id == spec_id) {
                continue;
            }
            refs.push(SpecRef {
                spec_id,
                file,
                anchor,
            });
        }
    }
    refs
}

/// The compiled SLD link pattern (shared by parse + scoping).
///
/// Why: compiling the regex once (lazily) keeps `parse_spec_refs` allocation-
/// free per call and is the workspace's accepted `OnceLock` exception.
/// What: returns the cached `Regex` matching ``[`SPEC-X-NN~vR`](docs/specs/
/// file.md#anchor)`` with groups (id, file, anchor).
/// Test: exercised by `super::tests::spec_resolve_parse_*`.
fn sld_link_re() -> &'static Regex {
    static LINK: OnceLock<Regex> = OnceLock::new();
    LINK.get_or_init(|| {
        Regex::new(
            r"\[`(SPEC-[A-Z0-9]+-\d+~[A-Za-z0-9]+)`\]\((docs/specs/[^)#]+)#([A-Za-z0-9~\-]+)\)",
        )
        .expect("SLD link pattern compiles")
    })
}

/// Extract the text of every `# Spec References` rustdoc block in `source`.
///
/// Why: §2.4 declares linkage in dedicated rustdoc blocks; honouring only those
/// blocks (not arbitrary text) is what stops the ISR inventing linkage from a
/// `SPEC-…` mention in unrelated prose (spec §6.4).
/// What: walks the source line by line, recognising a rustdoc heading line
/// (`//!`/`///` whose content is `# Spec References`, case-insensitive). Once
/// inside such a block, accumulates subsequent rustdoc-comment lines until a
/// non-rustdoc line or a different rustdoc heading terminates it. Returns the
/// joined body of each block (without comment markers).
/// Test: `super::tests::spec_resolve_parse_block_scoped`,
/// `spec_resolve_parse_ignores_non_block_ref`.
fn spec_reference_blocks(source: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for raw in source.lines() {
        match rustdoc_content(raw) {
            Some(content) => {
                let heading = content.trim_start_matches('#').trim();
                let is_spec_heading = content.trim_start().starts_with('#')
                    && heading.eq_ignore_ascii_case("Spec References");
                if is_spec_heading {
                    // A new `# Spec References` heading starts a fresh block.
                    if let Some(done) = current.take() {
                        blocks.push(done);
                    }
                    current = Some(String::new());
                } else if content.trim_start().starts_with("# ") {
                    // A different rustdoc heading ends the current block.
                    if let Some(done) = current.take() {
                        blocks.push(done);
                    }
                } else if let Some(buf) = current.as_mut() {
                    buf.push_str(content);
                    buf.push('\n');
                }
            }
            // Any non-rustdoc line terminates an open block.
            None => {
                if let Some(done) = current.take() {
                    blocks.push(done);
                }
            }
        }
    }
    if let Some(done) = current.take() {
        blocks.push(done);
    }
    blocks
}

/// Return a rustdoc comment line's content (after `//!`/`///`), or `None`.
///
/// Why: SLD blocks live only in rustdoc; distinguishing rustdoc from code/plain
/// comments is what scopes ref parsing to declared linkage (§2.4).
/// What: strips a leading `//!` or `///` (and one optional following space) and
/// returns the remainder; returns `None` for any non-rustdoc line.
/// Test: covered by `super::tests::spec_resolve_parse_block_scoped`.
fn rustdoc_content(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let body = trimmed
        .strip_prefix("//!")
        .or_else(|| trimmed.strip_prefix("///"))?;
    Some(body.strip_prefix(' ').unwrap_or(body))
}

/// Extract the spec-prescribed method from a spec markdown document.
///
/// Why: the C1 calling convention (`resolve::resolve_spec`) calls this thin
/// wrapper; C4 keeps the signature stable while routing through the hardened
/// [`resolve_spec_section`] so callers that only want the method are unchanged
/// (spec §6.4, §13 "C4 hardens the C1 seam").
/// What: resolves the anchor to its section (revision-tolerantly) and returns
/// the method lifted from its Behavior-Contract + Rationale prose, discarding
/// the drift metadata. Returns `None` when the anchor is absent or the section
/// prescribes no method.
/// Test: `super::tests::spec_resolve_method_*`.
#[must_use]
pub fn extract_spec_method(spec_markdown: &str, anchor: &str) -> Option<Method> {
    resolve_spec_section(spec_markdown, anchor).and_then(|r| r.method)
}

/// Resolve an SLD anchor to its governing section and lift the spec method.
///
/// Why: this is the C4 (#1361) hardened resolver — it scopes method extraction
/// to the Behavior-Contract + Rationale sub-blocks of the governed section
/// (§6.4) and reports revision drift so callers can flag `stale_spec`-adjacent
/// metadata without blocking (§6.4, OQ-6). OUTDATED enforcement is a non-goal
/// (§1.3) and is deliberately not implemented.
/// What: locates the section heading carrying `{#SPEC-…}` by matching the
/// **base id** (revision-insensitive), captures that section's body, extracts
/// the Behavior-Contract + Rationale prose, runs the shared method heuristic,
/// and compares the referencing anchor's revision against the section's to set
/// `revision_drift`. Returns `None` only when no section matches the base id.
/// Test: `super::tests::spec_resolve_drift_*`, `spec_resolve_method_*`,
/// `spec_resolve_section_*`.
#[must_use]
pub fn resolve_spec_section(spec_markdown: &str, anchor: &str) -> Option<SpecSectionResolution> {
    let (section, section_anchor) = section_body(spec_markdown, anchor)?;
    let section_revision = revision_of(&section_anchor);
    let ref_revision = revision_of(anchor);
    // Drift: both revisions are known and differ. A missing revision on either
    // side is not treated as drift (conservative — we only flag a *changed*
    // revision, never an absent one).
    let revision_drift = match (ref_revision.as_deref(), section_revision.as_deref()) {
        (Some(r), Some(s)) => r != s,
        _ => false,
    };

    let contract = behavior_contract_and_rationale(&section);
    let method = super::extract::heuristic_method(&contract).map(|mut m| {
        // Spec-sourced methods are advisory context; tag the kind but keep the
        // verbatim excerpt the heuristic captured.
        if matches!(m.kind, MethodKind::Unspecified) {
            m.kind = MethodKind::Approach;
        }
        m
    });

    Some(SpecSectionResolution {
        method,
        section_revision,
        revision_drift,
    })
}

/// Split a `SPEC-…~rev` id/anchor into its base id and revision.
///
/// Why: revision-drift detection (§6.4) and revision-tolerant section matching
/// both need to compare the *base* id (`SPEC-X-01`) independently of the
/// revision suffix (`draft`, `v1`, `v2`).
/// What: returns the substring after the last `~` as the revision, or `None`
/// when the id carries no `~` suffix.
/// Test: `super::tests::spec_resolve_revision_of`.
#[must_use]
pub fn revision_of(spec_id: &str) -> Option<String> {
    spec_id.rsplit_once('~').map(|(_, rev)| rev.to_string())
}

/// The revision-insensitive base of a `SPEC-…~rev` id/anchor.
///
/// Why: matching a referencing anchor (`SPEC-X-01~v1`) to a section heading
/// (`SPEC-X-01~v2`) must key on the stable base id, not the revision (§6.4
/// revision awareness).
/// What: returns the substring before the last `~`, or the whole id when there
/// is no `~`.
/// Test: covered by `super::tests::spec_resolve_drift_*`.
fn base_id(spec_id: &str) -> &str {
    spec_id.rsplit_once('~').map_or(spec_id, |(base, _)| base)
}

/// Return the body + actual anchor of the spec section matching `anchor`'s base.
///
/// Why: method extraction must be scoped to the *governed* section, matched
/// revision-tolerantly so a `~v1` ref still finds the `~v2` section (§6.4). An
/// unrelated method elsewhere in the spec must not be attributed to this change.
/// What: scans heading lines for a `{#SPEC-…}` marker whose **base id** equals
/// `anchor`'s base id; on match, returns the section body (up to the next
/// `## `/`# ` heading or `---` rule) together with the section's own full anchor
/// (so the caller can read its revision). Returns `None` when no section
/// matches the base id.
/// Test: `super::tests::spec_resolve_section_*`, `spec_resolve_drift_*`.
fn section_body(markdown: &str, anchor: &str) -> Option<(String, String)> {
    let want = base_id(anchor);
    let lines: Vec<&str> = markdown.lines().collect();

    let (start, section_anchor) = lines.iter().enumerate().find_map(|(i, l)| {
        anchor_in_heading(l)
            .filter(|a| base_id(a) == want)
            .map(|a| (i, a))
    })?;

    let mut body = String::new();
    for line in &lines[start + 1..] {
        let trimmed = line.trim_start();
        // A new `## ` subsection, a top-level `# ` section, or a `---` rule
        // terminates the current section. Breaking on `# ` too prevents a
        // following top-level section's prose from bleeding into this anchor's
        // method extraction.
        if trimmed.starts_with("## ") || trimmed.starts_with("# ") || trimmed == "---" {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    Some((body, section_anchor))
}

/// Extract the `{#SPEC-…}` anchor declared on a markdown heading line.
///
/// Why: section matching keys on the heading's `{#anchor}` marker; isolating
/// the parse keeps `section_body` readable and lets the test surface assert on
/// the anchor capture directly.
/// What: returns the `SPEC-…` id inside a `{#…}` marker on the line, or `None`
/// when the line carries no such marker.
/// Test: covered by `super::tests::spec_resolve_section_*`.
fn anchor_in_heading(line: &str) -> Option<String> {
    let open = line.find("{#")?;
    let rest = &line[open + 2..];
    let close = rest.find('}')?;
    Some(rest[..close].trim().to_string())
}

/// Lift the Behavior-Contract + Rationale prose out of a section body.
///
/// Why: §6.4 names the section's **Behavior Contract + Rationale** as the
/// spec-method source — not the whole section. Scoping extraction to those two
/// labelled sub-blocks avoids attributing prose from an unrelated sub-block
/// (e.g. an "Inputs" list) to the method.
/// What: scans the section for the bold labels `**Behavior Contract` and
/// `**Rationale` (case-insensitive, tolerant of the `(WHAT)`/`(WHY)` suffixes
/// and an optional trailing `:`), and accumulates the lines following each such
/// label until the next bold label or the end of the section. When the section
/// carries **neither** label, falls back to the whole section body (a spec
/// section that prescribes a method inline without the formal labels still
/// resolves — conservative for older/looser specs).
/// Test: `super::tests::spec_resolve_contract_scoped`,
/// `spec_resolve_contract_fallback`.
fn behavior_contract_and_rationale(section: &str) -> String {
    let mut out = String::new();
    let mut capturing = false;
    let mut saw_label = false;

    for line in section.lines() {
        if let Some(kind) = bold_label(line) {
            // A `**Behavior Contract**` / `**Rationale**` label turns capture
            // ON; any other bold label turns it OFF (we only want those two).
            capturing = kind;
            saw_label = true;
            continue;
        }
        if capturing {
            out.push_str(line);
            out.push('\n');
        }
    }

    if saw_label {
        out
    } else {
        // No formal labels in this section → fall back to the whole body so a
        // section that states its method inline still resolves.
        section.to_string()
    }
}

/// Classify a bold-label line as Behavior-Contract/Rationale (`Some(true)`),
/// some other bold label (`Some(false)`), or not a bold label (`None`).
///
/// Why: `behavior_contract_and_rationale` toggles capture on the two labels
/// §6.4 names and off on any other; a single classifier keeps that rule in one
/// place and auditable.
/// What: a line is a bold label when its trimmed form starts with `**`. It is a
/// Behavior-Contract/Rationale label when, after the `**`, it begins
/// (case-insensitively) with `behavior contract` or `rationale`.
/// Test: covered by `super::tests::spec_resolve_contract_scoped`.
fn bold_label(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    let inner = trimmed.strip_prefix("**")?;
    let lower = inner.to_ascii_lowercase();
    let is_target = lower.starts_with("behavior contract") || lower.starts_with("rationale");
    Some(is_target)
}
