//! Tests for `builder` — split out to keep `builder.rs` under the
//! 500-line hard cap enforced by `scripts/check_line_cap.sh`.
//!
//! Why: the implementation file (originally trusty-mpm's `agent_builder.rs`)
//! was approaching its frozen budget; the #389 regression tests would have
//! pushed it over, so the test module was extracted here following the same
//! pattern used elsewhere in the crate. Moved verbatim to
//! `trusty-agents-common::agents::builder_tests` alongside the implementation
//! in the #2892 extraction.
//! What: all unit and regression tests for [`compose_agent`] and
//! [`source_chain`], including colon-in-value round-trip checks.
//! Test: run with `cargo test -p trusty-agents-common -- agents::builder`.

use super::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Write `<name>.md` into `dir` with the given raw content.
fn write_agent(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(format!("{name}.md")), content).expect("write agent");
}

#[test]
fn compose_base_only() {
    // An agent with no `extends` returns its own body under a merged
    // frontmatter block — no inheritance to resolve.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nFoundation content.\n",
    );
    let composed = compose_agent("base-agent", tmp.path()).unwrap();
    assert!(composed.starts_with("---\n"));
    assert!(composed.contains("name: base-agent"));
    assert!(composed.contains("role: base"));
    assert!(composed.contains("Foundation content."));
    // `extends` must never leak into the composed frontmatter.
    assert!(!composed.contains("extends:"));
}

#[test]
fn compose_engineer_chain() {
    // engineer -> base-engineer -> base-agent must concatenate bodies
    // base-first and merge frontmatter child-wins.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Engineer\n\nENGINEER BASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\n---\n\n# Engineer\n\nLEAF BODY\n",
    );
    let composed = compose_agent("engineer", tmp.path()).unwrap();

    // Child fields win in the merged frontmatter.
    assert!(composed.contains("name: engineer"));
    assert!(composed.contains("role: engineer"));
    assert!(composed.contains("model: sonnet"));

    // Bodies appear base-first.
    let base = composed.find("BASE BODY").expect("base body present");
    let mid = composed
        .find("ENGINEER BASE BODY")
        .expect("base-engineer body present");
    let leaf = composed.find("LEAF BODY").expect("leaf body present");
    assert!(base < mid, "base body must precede base-engineer body");
    assert!(mid < leaf, "base-engineer body must precede leaf body");
}

#[test]
fn cycle_detection() {
    // A extends B, B extends A -> the chain forms a cycle.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "agent-a",
        "---\nname: agent-a\nextends: agent-b\n---\n\nA body\n",
    );
    write_agent(
        tmp.path(),
        "agent-b",
        "---\nname: agent-b\nextends: agent-a\n---\n\nB body\n",
    );
    let err = compose_agent("agent-a", tmp.path()).unwrap_err();
    match err {
        AgentBuildError::Cycle(chain) => {
            assert!(chain.contains(&"agent-a".to_string()));
            assert!(chain.contains(&"agent-b".to_string()));
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn depth_exceeded() {
    // A chain longer than MAX_DEPTH must fail with DepthExceeded.
    let tmp = TempDir::new().unwrap();
    // Build level0 (root) .. level10 each extending the previous.
    write_agent(tmp.path(), "level0", "---\nname: level0\n---\n\nroot\n");
    for i in 1..=10 {
        write_agent(
            tmp.path(),
            &format!("level{i}"),
            &format!(
                "---\nname: level{i}\nextends: level{}\n---\n\nbody{i}\n",
                i - 1
            ),
        );
    }
    let err = compose_agent("level10", tmp.path()).unwrap_err();
    assert!(
        matches!(err, AgentBuildError::DepthExceeded(MAX_DEPTH)),
        "expected DepthExceeded, got {err:?}"
    );
}

#[test]
fn compose_missing_agent() {
    // A request for a non-existent source file must surface NotFound.
    let tmp = TempDir::new().unwrap();
    let err = compose_agent("ghost", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "ghost"));
}

#[test]
fn missing_parent_is_not_found() {
    // A child extending an absent parent must report the parent missing.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "child",
        "---\nname: child\nextends: nowhere\n---\n\nbody\n",
    );
    let err = compose_agent("child", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::NotFound(name) if name == "nowhere"));
}

#[test]
fn unterminated_frontmatter_errors() {
    // A frontmatter block missing its closing `---` is a parse error.
    let tmp = TempDir::new().unwrap();
    write_agent(tmp.path(), "broken", "---\nname: broken\n\n# No close\n");
    let err = compose_agent("broken", tmp.path()).unwrap_err();
    assert!(matches!(err, AgentBuildError::FrontmatterParse(_)));
}

#[test]
fn source_chain_engineer() {
    // The resolved chain must list ancestors base-first.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\n---\n\nb\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nextends: base-agent\n---\n\nbe\n",
    );
    write_agent(
        tmp.path(),
        "engineer",
        "---\nname: engineer\nextends: base-engineer\n---\n\ne\n",
    );
    let chain = source_chain("engineer", tmp.path()).unwrap();
    assert_eq!(chain, vec!["base-agent", "base-engineer", "engineer"]);
}

#[test]
fn source_chain_base_only() {
    // A base agent's chain is just itself.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\n---\n\nb\n",
    );
    let chain = source_chain("base-agent", tmp.path()).unwrap();
    assert_eq!(chain, vec!["base-agent"]);
}

// ── colon-in-value regression tests (issue #389) ─────────────────────────

#[test]
fn url_value_round_trips() {
    // A value that is a URL (`https://...`) contains multiple colons; the
    // parser must split on the FIRST colon only and preserve the full URL.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "web-agent",
        "---\nname: web-agent\nrole: web\nmodel: https://example.com/model-api\n---\n\n# Web\n",
    );
    // compose_agent must not error; the URL must survive round-trip.
    let composed = compose_agent("web-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("model: https://example.com/model-api"),
        "URL value must be preserved verbatim; got:\n{composed}"
    );
}

#[test]
fn timestamp_value_round_trips() {
    // ISO-8601 timestamps (`2026-06-05T14:31:34`) contain colons in the
    // time component; the parser must keep the entire timestamp.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "ts-agent",
        "---\nname: ts-agent\nrole: worker\ndescription: Created at 2026-06-05T14:31:34\n---\n\n# TS\n",
    );
    let composed = compose_agent("ts-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("2026-06-05T14:31:34"),
        "timestamp in description must survive; got:\n{composed}"
    );
}

#[test]
fn bedrock_model_id_round_trips() {
    // Model ids like `bedrock/us.anthropic.claude-sonnet-4-6` contain
    // slashes and dots; the full id must survive the round-trip without
    // truncation at any embedded separator.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "ai-agent",
        "---\nname: ai-agent\nrole: ai\nmodel: bedrock/us.anthropic.claude-sonnet-4-6\n---\n\n# AI\n",
    );
    let composed = compose_agent("ai-agent", tmp.path()).unwrap();
    assert!(
        composed.contains("model: bedrock/us.anthropic.claude-sonnet-4-6"),
        "model id must be preserved; got:\n{composed}"
    );
}

// ── case-insensitive resolution regression test (issue #790) ─────────────────
//
// Root cause: base template files on disk are named `BASE-QA.md`,
// `BASE-ENGINEER.md`, etc. (UPPERCASE stems), but concrete agents declare
// `extends: base-qa` / `extends: base-engineer` (lowercase). On macOS
// (case-insensitive HFS+) the OS silently matched these; on Linux
// (case-sensitive ext4) it failed with `agent source not found: base-qa`.
//
// Fix: `build_source_map` keys files by their LOWERCASED stem, so the
// resolver looks up `name.to_lowercase()` and finds the UPPERCASE file
// regardless of the host filesystem's case behaviour.
//
// This test MUST NOT rely on filesystem case-folding to prove correctness.
// It writes the base file with an UPPERCASE stem (`BASE-QA.md`) and the
// concrete agent with `extends: base-qa` (lowercase), then asserts that
// composition succeeds. On a case-SENSITIVE filesystem this would fail
// with the old direct-path code but passes with the new map-based lookup.

// ── HR-1 deploy-time enrichment tests (issue #1406) ──────────────────────────

#[test]
fn tier_model_mapping() {
    // An agent declaring `resource_tier: intensive` with no explicit `model`
    // must receive the tier-derived default (`opus`). Mirrors claude-mpm's
    // RESOURCE_TIER_DEFAULTS.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "heavy",
        "---\nname: heavy\nrole: engineer\nresource_tier: intensive\n---\n\n# Heavy\n",
    );
    let composed = compose_agent("heavy", tmp.path()).unwrap();
    assert!(
        composed.contains("model: opus"),
        "intensive tier must map to opus; got:\n{composed}"
    );
    // The tier itself is preserved for downstream consumers.
    assert!(composed.contains("resource_tier: intensive"));
}

#[test]
fn tier_lightweight_maps_to_haiku() {
    // `lightweight` → haiku.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "light",
        "---\nname: light\nrole: ops\nresource_tier: lightweight\n---\n\n# Light\n",
    );
    let composed = compose_agent("light", tmp.path()).unwrap();
    assert!(
        composed.contains("model: haiku"),
        "lightweight tier must map to haiku; got:\n{composed}"
    );
}

#[test]
fn tier_high_and_standard_map_to_sonnet() {
    // Both `high` and `standard` map to sonnet.
    let tmp = TempDir::new().unwrap();
    for (name, tier) in [("hi", "high"), ("std", "standard")] {
        write_agent(
            tmp.path(),
            name,
            &format!("---\nname: {name}\nrole: engineer\nresource_tier: {tier}\n---\n\n# {name}\n"),
        );
        let composed = compose_agent(name, tmp.path()).unwrap();
        assert!(
            composed.contains("model: sonnet"),
            "{tier} tier must map to sonnet; got:\n{composed}"
        );
    }
}

#[test]
fn tier_unknown_falls_back_to_sonnet() {
    // An unrecognised tier falls back to the standard default (sonnet), per the
    // HR-1 error contract.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "weird",
        "---\nname: weird\nrole: engineer\nresource_tier: galaxy-brain\n---\n\n# Weird\n",
    );
    let composed = compose_agent("weird", tmp.path()).unwrap();
    assert!(
        composed.contains("model: sonnet"),
        "unknown tier must fall back to sonnet; got:\n{composed}"
    );
}

#[test]
fn explicit_model_wins_over_tier() {
    // An explicit `model` must override the tier-derived default — explicit
    // always wins, even when the tier would map to something else.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "pinned",
        "---\nname: pinned\nrole: engineer\nresource_tier: intensive\nmodel: haiku\n---\n\n# Pinned\n",
    );
    let composed = compose_agent("pinned", tmp.path()).unwrap();
    assert!(
        composed.contains("model: haiku"),
        "explicit model must win over tier-derived opus; got:\n{composed}"
    );
    assert!(
        !composed.contains("model: opus"),
        "tier-derived model must not appear when explicit model is set"
    );
}

#[test]
fn no_tier_no_model_emits_no_model() {
    // With neither tier nor explicit model, no `model:` line is synthesised —
    // the enrichment only fires when a tier is present.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "bare",
        "---\nname: bare\nrole: base\n---\n\n# Bare\n",
    );
    let composed = compose_agent("bare", tmp.path()).unwrap();
    assert!(
        !composed.contains("model:"),
        "no model should be emitted without tier or explicit model; got:\n{composed}"
    );
}

#[test]
fn initial_prompt_injected_by_role() {
    // An engineer-role agent that declares no initialPrompt must receive the
    // engineer default at compose time.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "eng",
        "---\nname: eng\nrole: engineer\nmodel: sonnet\n---\n\n# Eng\n",
    );
    let composed = compose_agent("eng", tmp.path()).unwrap();
    assert!(
        composed.contains(r#"initialPrompt: "Begin implementation."#),
        "engineer role must get the implementation initialPrompt; got:\n{composed}"
    );
}

#[test]
fn initial_prompt_role_qa_and_ops() {
    // qa and ops roles get their type-specific prompts.
    let tmp = TempDir::new().unwrap();
    write_agent(tmp.path(), "q", "---\nname: q\nrole: qa\n---\n\n# Q\n");
    write_agent(tmp.path(), "o", "---\nname: o\nrole: ops\n---\n\n# O\n");
    let q = compose_agent("q", tmp.path()).unwrap();
    let o = compose_agent("o", tmp.path()).unwrap();
    assert!(
        q.contains(r#"initialPrompt: "Begin verification."#),
        "qa prompt:\n{q}"
    );
    assert!(
        o.contains(r#"initialPrompt: "Begin operations."#),
        "ops prompt:\n{o}"
    );
}

#[test]
fn initial_prompt_excluded_role() {
    // Special-purpose / interactive roles (e.g. `base`, `ticketing`) must NOT
    // get an injected initialPrompt — they wait for direction.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "mgr",
        "---\nname: mgr\nrole: base\nmodel: sonnet\n---\n\n# Mgr\n",
    );
    let composed = compose_agent("mgr", tmp.path()).unwrap();
    assert!(
        !composed.contains("initialPrompt:"),
        "excluded role must not get an initialPrompt; got:\n{composed}"
    );
}

#[test]
fn initial_prompt_explicit_wins() {
    // A source-declared initialPrompt must survive verbatim and override the
    // role-derived default.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "custom",
        "---\nname: custom\nrole: engineer\ninitialPrompt: Do the special thing first.\n---\n\n# Custom\n",
    );
    let composed = compose_agent("custom", tmp.path()).unwrap();
    assert!(
        composed.contains(r#"initialPrompt: "Do the special thing first.""#),
        "explicit initialPrompt must win; got:\n{composed}"
    );
    assert!(
        !composed.contains("Begin implementation."),
        "role default must not appear when an explicit prompt is set"
    );
}

#[test]
fn enrichment_survives_inheritance_chain() {
    // tier and role come from the child; the enrichment must apply to the fully
    // composed chain, and base content must still be present.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\n---\n\n# Base\n\nBASE GUARANTEE\n",
    );
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nrole: base-engineer\nextends: base-agent\n---\n\n# Base Eng\n\nENG GUARANTEE\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-engineer\nresource_tier: intensive\n---\n\n# Leaf\n\nLEAF GUARANTEE\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    // Enrichments applied from the merged (child-wins) frontmatter.
    assert!(
        composed.contains("model: opus"),
        "tier→model on chain:\n{composed}"
    );
    assert!(
        composed.contains(r#"initialPrompt: "Begin implementation."#),
        "role-derived prompt on chain:\n{composed}"
    );
    // Inherited base content still present.
    assert!(composed.contains("BASE GUARANTEE"));
    assert!(composed.contains("ENG GUARANTEE"));
    assert!(composed.contains("LEAF GUARANTEE"));
}

#[test]
fn initial_prompt_with_embedded_quote_round_trips() {
    // A source initialPrompt containing an embedded double-quote must emit
    // valid, parseable frontmatter and survive a compose→re-compose cycle
    // verbatim (the embedded quote is escaped on emit, decoded on re-parse).
    let tmp = TempDir::new().unwrap();
    let original = r#"Run the "fast path" then stop."#;
    write_agent(
        tmp.path(),
        "quoted",
        &format!("---\nname: quoted\nrole: engineer\ninitialPrompt: {original}\n---\n\n# Quoted\n"),
    );

    let composed = compose_agent("quoted", tmp.path()).unwrap();
    // Emitted line must escape the inner quotes — never a bare `"` that would
    // prematurely close the YAML scalar.
    assert!(
        composed.contains(r#"initialPrompt: "Run the \"fast path\" then stop.""#),
        "embedded quote must be escaped in emitted frontmatter; got:\n{composed}"
    );

    // Round-trip: feed the composed file back through compose_agent. The
    // emitted camelCase key re-parses, the escapes decode, and the prompt is
    // recovered unchanged.
    write_agent(tmp.path(), "quoted2", &composed);
    let (fm, _) = split_frontmatter(&composed).expect("composed frontmatter must re-parse");
    assert_eq!(
        fm.initial_prompt.as_deref(),
        Some(original),
        "initialPrompt must survive the compose→re-parse cycle verbatim"
    );

    // And a full re-compose of the round-tripped file emits the same line.
    let recomposed = compose_agent("quoted2", tmp.path()).unwrap();
    assert!(
        recomposed.contains(r#"initialPrompt: "Run the \"fast path\" then stop.""#),
        "re-composed frontmatter must be stable; got:\n{recomposed}"
    );
}

#[test]
fn initial_prompt_with_backslash_round_trips() {
    // A backslash in the prompt must be escaped (`\` → `\\`) on emit and
    // decoded on re-parse so a path-like or regex-like prompt survives.
    let tmp = TempDir::new().unwrap();
    let original = r#"Use the C:\path and the \d+ pattern."#;
    write_agent(
        tmp.path(),
        "slash",
        &format!("---\nname: slash\nrole: engineer\ninitialPrompt: {original}\n---\n\n# Slash\n"),
    );
    let composed = compose_agent("slash", tmp.path()).unwrap();
    let (fm, _) = split_frontmatter(&composed).expect("composed frontmatter must re-parse");
    assert_eq!(
        fm.initial_prompt.as_deref(),
        Some(original),
        "backslash-bearing initialPrompt must round-trip verbatim; got:\n{composed}"
    );
}

#[test]
fn initial_prompt_role_default_round_trips() {
    // The role-derived default prompt (no source initialPrompt) must also be
    // stable across a compose→re-compose cycle, confirming the in-lower /
    // out-camel key convention round-trips.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "eng",
        "---\nname: eng\nrole: engineer\n---\n\n# Eng\n",
    );
    let composed = compose_agent("eng", tmp.path()).unwrap();
    let (fm, _) = split_frontmatter(&composed).expect("composed frontmatter must re-parse");
    assert_eq!(
        fm.initial_prompt.as_deref(),
        Some("Begin implementation. Read the task context and start coding immediately."),
        "role default initialPrompt must re-parse unchanged; got:\n{composed}"
    );
    // Re-composing the round-tripped file yields identical frontmatter for the
    // initialPrompt line (camelCase key, escaped scalar).
    write_agent(tmp.path(), "eng2", &composed);
    let recomposed = compose_agent("eng2", tmp.path()).unwrap();
    let first_line = "initialPrompt: \"Begin implementation.";
    assert!(
        composed.contains(first_line) && recomposed.contains(first_line),
        "initialPrompt line must be stable across the cycle"
    );
}

#[test]
fn case_insensitive_resolve_via_map() {
    // Write `BASE-QA.md` (UPPERCASE stem, as shipped in the bundle) and a
    // concrete `qa.md` that extends it via the lowercase name `base-qa`.
    // The SourceMap must bridge the case gap so compose_agent succeeds on
    // both case-sensitive (Linux) and case-insensitive (macOS) filesystems.
    let tmp = TempDir::new().unwrap();

    // Write the base file with an UPPERCASE stem — matching the real asset name.
    fs::write(
        tmp.path().join("BASE-QA.md"),
        "---\nname: base-qa\nrole: base-qa\n---\n\n# Base QA\n\nBASE QA BODY\n",
    )
    .expect("write BASE-QA.md");

    // Concrete agent uses lowercase in extends — matching the real qa.md.
    write_agent(
        tmp.path(),
        "qa",
        "---\nname: qa\nrole: qa\nextends: base-qa\n---\n\n# QA Agent\n\nQA BODY\n",
    );

    // Verify the SourceMap correctly maps "base-qa" -> BASE-QA.md path.
    let map = build_source_map(tmp.path());
    assert!(
        map.contains_key("base-qa"),
        "SourceMap must contain 'base-qa' key (from BASE-QA.md); map keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        map["base-qa"].ends_with("BASE-QA.md"),
        "SourceMap['base-qa'] must point to BASE-QA.md, got {:?}",
        map["base-qa"]
    );

    // The composition must succeed: lowercase extends-value finds UPPERCASE file.
    let composed = compose_agent("qa", tmp.path())
        .expect("compose_agent must succeed: base-qa (lowercase) -> BASE-QA.md (uppercase)");

    // Base body appears before the concrete agent body.
    let base_pos = composed.find("BASE QA BODY").expect("base body present");
    let qa_pos = composed.find("QA BODY").expect("qa body present");
    assert!(
        base_pos < qa_pos,
        "base body must precede concrete agent body"
    );

    // Merged frontmatter carries the child's name.
    assert!(
        composed.contains("name: qa"),
        "merged frontmatter has child name"
    );
    // `extends:` must not leak into the output.
    assert!(
        !composed.contains("extends:"),
        "extends must not appear in output"
    );
}

// ── DOC-42 (issue #2889) — `skills:` frontmatter parsing & merge ───────────

#[test]
fn compose_agent_with_no_skills_omits_the_key() {
    // Absent `skills:` must parse to empty and never appear in the composed
    // output — exactly today's behavior (backward compatibility).
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nrole: engineer\n---\n\n# Plain\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path()).unwrap();
    assert!(!composed.contains("skills:"));
}

#[test]
fn compose_agent_declares_skills_inline_array() {
    // The normative grammar (DOC-42 §SPEC-AGENTSKILLS-01): a bracketed,
    // comma-separated inline array on the `skills:` line.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "critic",
        "---\nname: critic\nrole: qa\nskills: [code-review-standards, systematic-debugging]\n---\n\n# Critic\n\nBODY\n",
    );
    let composed = compose_agent("critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [code-review-standards, systematic-debugging]"));
}

#[test]
fn compose_agent_empty_skills_array_omits_the_key() {
    // `skills: []` is documented as equivalent to omission.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nrole: engineer\nskills: []\n---\n\n# Plain\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path()).unwrap();
    assert!(!composed.contains("skills:"));
}

#[test]
fn skills_union_across_chain() {
    // A child agent's declared skills merge with its base's — both survive
    // in the composed output (list fields union, unlike scalar overrides).
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-qa",
        "---\nname: base-qa\nrole: base-qa\nskills: [verification-before-completion]\n---\n\n# Base QA\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "code-critic",
        "---\nname: code-critic\nrole: qa\nextends: base-qa\nskills: [code-review-standards]\n---\n\n# Critic\n\nLEAF BODY\n",
    );
    let composed = compose_agent("code-critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [verification-before-completion, code-review-standards]"));
}

#[test]
fn skills_deduplicated_across_chain() {
    // The same skill declared by both base and child must appear once, at
    // its first-seen (base-first) position.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-qa",
        "---\nname: base-qa\nrole: base-qa\nskills: [shared-skill]\n---\n\n# Base QA\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "code-critic",
        "---\nname: code-critic\nrole: qa\nextends: base-qa\nskills: [shared-skill, unique-skill]\n---\n\n# Critic\n\nLEAF BODY\n",
    );
    let composed = compose_agent("code-critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [shared-skill, unique-skill]"));
    // Must not appear twice.
    assert_eq!(composed.matches("shared-skill").count(), 1);
}

#[test]
fn compose_agent_matches_sibling_2890_code_critic_fixture() {
    // Cross-PR compatibility fixture (issue #2890, PR #2900 on
    // `feat-2890-code-critic-skills`): the sibling agent declares
    // `code-critic.md`'s `skills:` using the exact same inline flow-style
    // array syntax this parser implements —
    // `skills: [code-review-standards, contract-driven-testing]`. This test
    // pins that EXACT syntax (not the real bundled `code-critic.md`, which
    // lives on a branch not yet merged to main) so #2889's parser and
    // #2890's asset content compose correctly once both land.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "code-critic",
        "---\nname: code-critic\nrole: qa\nskills: [code-review-standards, contract-driven-testing]\n---\n\n# Code Critic\n\nBODY\n",
    );
    let composed = compose_agent("code-critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [code-review-standards, contract-driven-testing]"));
}

#[test]
fn compose_agent_parses_block_style_skills_list() {
    // Issue #2906 review (CRITICAL): DOC-42 §1.1's motivating example (and
    // every realistic upstream `claude-mpm-agents` asset) declares `skills:`
    // as a YAML BLOCK sequence, not the inline flow array:
    //   skills:
    //     - code-review-standards
    //     - code-production-process
    // This must compose successfully (not error) and populate BOTH items.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "code-critic",
        "---\nname: code-critic\nrole: qa\nskills:\n  - code-review-standards\n  - code-production-process\n---\n\n# Code Critic\n\nBODY\n",
    );
    let composed = compose_agent("code-critic", tmp.path());
    assert!(
        composed.is_ok(),
        "block-style skills list must compose successfully, got: {composed:?}"
    );
    let composed = composed.unwrap();
    assert!(composed.contains("skills: [code-review-standards, code-production-process]"));
}

#[test]
fn compose_agent_block_style_skills_survives_inheritance() {
    // The block-style form must union across `extends:` exactly like the
    // inline flow form does.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-qa",
        "---\nname: base-qa\nrole: base-qa\nskills:\n  - verification-before-completion\n---\n\n# Base QA\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "code-critic",
        "---\nname: code-critic\nrole: qa\nextends: base-qa\nskills:\n  - code-review-standards\n---\n\n# Critic\n\nLEAF BODY\n",
    );
    let composed = compose_agent("code-critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [verification-before-completion, code-review-standards]"));
}

#[test]
fn compose_agent_block_style_skills_with_trailing_content() {
    // A block sequence followed by MORE frontmatter keys (not immediately the
    // closing fence) must still parse: the block ends on the first
    // non-`-`/non-blank line, and that line is then parsed normally.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "critic",
        "---\nname: critic\nskills:\n  - code-review-standards\n  - systematic-debugging\nmodel: sonnet\n---\n\nBODY\n",
    );
    let composed = compose_agent("critic", tmp.path()).unwrap();
    assert!(composed.contains("skills: [code-review-standards, systematic-debugging]"));
    assert!(composed.contains("model: sonnet"));
}

#[test]
fn compose_agent_empty_block_style_skills_omits_the_key() {
    // A bare `skills:` with NO continuation items (block never populated)
    // must degrade to empty — equivalent to omission — not error.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nskills:\nmodel: sonnet\n---\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path());
    assert!(composed.is_ok(), "empty block-style skills must not error");
    assert!(!composed.unwrap().contains("skills:"));
}

#[test]
fn skills_malformed_value_is_treated_as_empty() {
    // A `skills:` line whose value doesn't look like a list (e.g. blank)
    // must never hard-error the compose — it parses to an empty list.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nrole: engineer\nskills:\n---\n\n# Plain\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path());
    assert!(composed.is_ok(), "malformed skills value must not error");
    assert!(!composed.unwrap().contains("skills:"));
}

#[test]
fn skills_malformed_inline_value_never_errors() {
    // Issue #2906 review (MEDIUM finding): a genuinely unparseable inline
    // value (comma(s) with no real elements) must still degrade to an empty
    // list rather than error — the fix adds a `tracing::warn!` at this call
    // site (not directly assertable in a unit test without a subscriber),
    // but the never-halt contract IS assertable here.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nskills: ,\n---\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path());
    assert!(composed.is_ok(), "malformed inline skills must not error");
    assert!(!composed.unwrap().contains("skills:"));
}

// ── #2897: max_tokens + tools (tcode agent-format unification, epic #2892) ─

#[test]
fn max_tokens_parses_from_frontmatter() {
    // A bare `max_tokens:` scalar must parse and emit unchanged when there
    // is no `extends` chain to merge across.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "solo",
        "---\nname: solo\nrole: engineer\nmax_tokens: 4096\n---\n\n# Solo\n",
    );
    let composed = compose_agent("solo", tmp.path()).unwrap();
    assert!(composed.contains("max_tokens: 4096"), "got:\n{composed}");
}

#[test]
fn max_tokens_child_wins_across_chain() {
    // Scalar child-wins merge, mirroring `model:` — the child's explicit
    // value overrides the parent's.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\nmax_tokens: 8192\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\nmax_tokens: 4096\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    assert!(
        composed.contains("max_tokens: 4096"),
        "child's max_tokens must win; got:\n{composed}"
    );
    assert!(!composed.contains("max_tokens: 8192"));
}

#[test]
fn max_tokens_unset_child_inherits_parent() {
    // A child that omits `max_tokens:` must inherit the parent's value
    // rather than clearing it — same child-wins-only-when-set semantics as
    // `model:`.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\nmax_tokens: 8192\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    assert!(
        composed.contains("max_tokens: 8192"),
        "child must inherit parent's max_tokens when unset; got:\n{composed}"
    );
}

#[test]
fn max_tokens_malformed_value_is_dropped_not_panicking() {
    // Code-critic finding on PR #2952 (MEDIUM): a non-numeric, negative, or
    // out-of-range `max_tokens:` value must warn-and-drop (parse to `None`)
    // rather than panicking or hard-erroring the compose — mirroring
    // `skills_malformed_inline_value_never_errors`.
    let tmp = TempDir::new().unwrap();
    for (name, raw) in [
        ("non-numeric", "not-a-number"),
        ("negative", "-42"),
        ("too-large", "99999999999999999999"), // far beyond u32::MAX
    ] {
        write_agent(
            tmp.path(),
            name,
            &format!("---\nname: {name}\nrole: engineer\nmax_tokens: {raw}\n---\n\n# {name}\n"),
        );
        let composed = compose_agent(name, tmp.path());
        assert!(
            composed.is_ok(),
            "malformed max_tokens ({name} = {raw:?}) must not error"
        );
        assert!(
            !composed.unwrap().contains("max_tokens:"),
            "malformed max_tokens ({name} = {raw:?}) must drop to None, not \
             synthesise a `max_tokens:` line"
        );
    }
}

#[test]
fn tools_parses_as_a_list() {
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "solo",
        "---\nname: solo\nrole: engineer\ntools: [read, write, bash]\n---\n\n# Solo\n",
    );
    let composed = compose_agent("solo", tmp.path()).unwrap();
    assert!(
        composed.contains("tools: [read, write, bash]"),
        "got:\n{composed}"
    );
}

#[test]
fn tools_malformed_inline_value_never_errors() {
    // Mirrors `skills_malformed_inline_value_never_errors`: a genuinely
    // unparseable inline value (comma with no real elements) must never
    // hard-error the compose. Since the `tools:` key IS present (even though
    // malformed), it parses to `Some(vec![])` — same as an explicit
    // `tools: []` — which is the correct, safe degrade: a garbled
    // restriction is not silently discarded back into "inherit everything".
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "plain",
        "---\nname: plain\nrole: engineer\ntools: ,\n---\n\nBODY\n",
    );
    let composed = compose_agent("plain", tmp.path());
    assert!(composed.is_ok(), "malformed inline tools must not error");
    assert!(
        composed.unwrap().contains("tools: []"),
        "a malformed but present `tools:` key must degrade to an explicit \
         empty override, not silently vanish"
    );
}

#[test]
fn tools_override_across_chain() {
    // Unlike `skills:`, `tools:` REPLACES the parent's list entirely when
    // the child declares a non-empty `tools:` — a restrictive leaf must be
    // able to narrow a permissive base's tool set.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\ntools: [x, y, z]\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\ntools: [a, b]\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    assert!(
        composed.contains("tools: [a, b]"),
        "child's tools must replace the parent's; got:\n{composed}"
    );
    assert!(!composed.contains("x, y, z"));
}

#[test]
fn tools_unset_child_inherits_parent() {
    // A child that omits `tools:` must inherit the parent's list unchanged.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\ntools: [x, y, z]\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    assert!(
        composed.contains("tools: [x, y, z]"),
        "child must inherit parent's tools when unset; got:\n{composed}"
    );
}

#[test]
fn tools_empty_list_overrides_to_deny_all() {
    // Code-critic finding on PR #2952 (MEDIUM): `tools: []` must be a
    // distinguishable, explicit deny-all override — NOT collapse into the
    // same "inherit the parent" behaviour an omitted `tools:` key produces.
    // A non-empty parent list must be overridden down to zero, and this must
    // be provably different from `tools_unset_child_inherits_parent` above
    // (same parent, different child declaration, opposite outcome).
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\ntools: [x, y, z]\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\ntools: []\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    assert!(
        composed.contains("tools: []"),
        "an explicit empty `tools:` must override the parent's list down to \
         zero (deny-all), not inherit; got:\n{composed}"
    );
    assert!(
        !composed.contains("x, y, z"),
        "the parent's tools must not survive an explicit `tools: []` \
         override; got:\n{composed}"
    );
}

#[test]
fn tools_override_contrasts_with_skills_union() {
    // Same chain shape, both a `skills:` and a `tools:` declaration at each
    // level — `skills:` must UNION (both survive) while `tools:` must
    // OVERRIDE (only the child's survive). Proves the two list fields do not
    // share merge logic.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-agent",
        "---\nname: base-agent\nrole: base\nskills: [base-skill]\ntools: [base-tool]\n---\n\n# Base\n",
    );
    write_agent(
        tmp.path(),
        "leaf",
        "---\nname: leaf\nrole: engineer\nextends: base-agent\nskills: [leaf-skill]\ntools: [leaf-tool]\n---\n\n# Leaf\n",
    );
    let composed = compose_agent("leaf", tmp.path()).unwrap();
    // skills: union — both survive.
    assert!(
        composed.contains("skills: [base-skill, leaf-skill]"),
        "skills must union; got:\n{composed}"
    );
    // tools: override — only the child's tool survives.
    assert!(
        composed.contains("tools: [leaf-tool]"),
        "tools must override, not union; got:\n{composed}"
    );
    assert!(
        !composed.contains("base-tool"),
        "the parent's tool must not survive the override; got:\n{composed}"
    );
}

#[test]
fn no_max_tokens_no_tools_agent_composes_unchanged() {
    // Behavior-preservation for tm (#2897): a tm-style agent that never sets
    // `max_tokens:`/`tools:` must compose byte-identically to the pre-#2897
    // output — no `max_tokens:` or `tools:` line is synthesised, and no
    // other field is perturbed.
    let tmp = TempDir::new().unwrap();
    write_agent(
        tmp.path(),
        "base-engineer",
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Engineer\n\nBASE BODY\n",
    );
    write_agent(
        tmp.path(),
        "rust-engineer",
        "---\nname: rust-engineer\nrole: engineer\nextends: base-engineer\nmodel: sonnet\nskills: [toolchains-rust-core]\n---\n\n# Rust Engineer\n\nLEAF BODY\n",
    );
    let composed = compose_agent("rust-engineer", tmp.path()).unwrap();
    let expected = "---\nname: rust-engineer\nrole: engineer\nmodel: sonnet\nskills: [toolchains-rust-core]\ninitialPrompt: \"Begin implementation. Read the task context and start coding immediately.\"\n---\n\n# Base Engineer\n\nBASE BODY\n\n# Rust Engineer\n\nLEAF BODY\n";
    assert_eq!(
        composed, expected,
        "tm-style agent (no max_tokens/tools) must compose byte-identically \
         to pre-#2897 output"
    );
    assert!(!composed.contains("max_tokens:"));
    assert!(!composed.contains("tools:"));
}
