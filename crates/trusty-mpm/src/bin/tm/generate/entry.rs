//! Renders the `tm-capabilities.md` entry skill file (issue #2913).
//!
//! Why: the entry file is the thing every agent actually loads first — it
//! must stay short (~100 lines), state its own provenance so nobody
//! hand-edits generated content, and route the reader to the right
//! `references/*.md` file rather than dumping everything inline.
//! What: [`render`] emits the YAML frontmatter (mirroring every other
//! bundled skill's shape) plus a short body: what this harness catalog is,
//! the provenance/regeneration banner, one line per `references/*.md` file
//! with its row count, the relationship to the hand-authored `tm` skill
//! (complement, not supersede — issue #2913 brief §C), and a pointer to the
//! hand-authored `references/workflows.md`.
//! Test: `entry_render_has_frontmatter`, `entry_render_mentions_provenance`.

use clap::CommandFactory;

use crate::cli::Cli;

/// Render the `tm-capabilities.md` entry file.
///
/// Why: counts are computed live (not hard-coded) so the entry file's
/// headline numbers can never drift from the reference files generated
/// alongside it in the same run.
/// What: takes the already-rendered reference file bodies only to derive
/// stable counts (top-level CLI commands, MCP tools, agents, skills, doctor
/// checks) — never re-parses them, just counts from the same source data
/// each reference renderer reads.
/// Test: `entry_render_has_frontmatter`, `entry_render_mentions_provenance`.
pub(crate) fn render() -> String {
    let cli_count = Cli::command().get_subcommands().count();
    let mcp_count = trusty_mpm::mcp::tools::tool_catalog().len();
    let agent_count = trusty_mpm::core::bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("agents/") && !a.rel_path.starts_with("agents/BASE-"))
        .count();
    let skill_count = trusty_mpm::core::bundle::ALL
        .iter()
        .filter(|a| super::skills::is_top_level_skill(a.rel_path))
        .count();
    let doctor_count = super::doctor::DOCTOR_CHECKS.len();

    format!(
        r#"---
name: tm-capabilities
description: Auto-generated exhaustive harness capability catalog — every tm CLI command, MCP tool, bundled agent, bundled skill, and doctor check, verbatim and always current. Complements (does not replace) the conceptual `tm` skill.
user-invocable: false
version: "1.0.0"
category: pm-reference
tags: [reference, generated, cli, mcp, agents, skills, doctor]
effort: low
---

# tm-capabilities — Generated Harness Capability Catalog

> **AUTO-GENERATED — do not hand-edit.** This file and its `references/
> {{cli,mcp-tools,agents,skills,doctor}}.md` siblings are produced by
> `tm generate capabilities` from the harness's own in-process data (clap's
> command-tree introspection, the MCP tool catalog, the bundled agent/skill
> roster, and a maintained doctor-check list cross-checked against
> `run_doctor`'s real output). A CI drift gate
> (`scripts/check_capabilities.sh`, wired into `.github/workflows/`) fails
> the build if the committed files ever fall out of sync with the generator's
> output. To change what this skill says, change the harness surface it
> describes, then run `tm generate capabilities` and commit the diff.
> `references/workflows.md` is the one exception — it is hand-authored.

## What This Is

An exhaustive, exact reference for "what exact surface exists, verbatim,
right now" — every CLI command, MCP tool schema, agent description, skill
description, and doctor-check meaning. This is deliberately NOT the same
document as the `tm` skill: `tm` is a *conceptual* overview (the delegation
model, memory system, "how it works" prose) for building intuition;
`tm-capabilities` is a *reference* catalog for looking up an exact name,
signature, or meaning. Load `tm` first to understand the system; load the
relevant `references/*.md` file here when you need an exact answer.

## When to Consult Which Reference

| Question | Load |
|---|---|
| "What are the exact `tm <command>` subcommands / flags?" | `references/cli.md` ({cli_count} top-level commands) |
| "What MCP tools exist and what parameters do they take?" | `references/mcp-tools.md` ({mcp_count} tools, plus sibling-daemon pointers) |
| "What agents can I delegate to, and what do they declare?" | `references/agents.md` ({agent_count} concrete agents) |
| "What skills exist, and are they user-invocable?" | `references/skills.md` ({skill_count} bundled skills) |
| "What does a `tm doctor` check name actually mean?" | `references/doctor.md` ({doctor_count} checks) |
| "How do the pieces fit into an end-to-end flow?" | `references/workflows.md` (hand-authored, not generated) |

## Relationship to Other Skills

- **`tm`** — conceptual overview; keeps its own prose roster/skill list and
  points here for the exhaustive, generated version (issue #2913 brief §C:
  complement, not supersede — avoids two hand/auto-maintained lists
  disagreeing).
- **`tm-doctor`**, **`tm-cli-operations`**, **`tm-agent-architecture`** —
  narrowly procedural companions; this skill is the data they operate on,
  not a replacement for their guidance.

## Regenerating

```bash
tm generate capabilities          # write the regenerated files
tm generate capabilities --check  # CI drift gate: diff only, nonzero on mismatch
```
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_render_has_frontmatter() {
        let rendered = render();
        assert!(
            rendered.starts_with("---\nname: tm-capabilities\n"),
            "{rendered}"
        );
        assert!(rendered.contains("user-invocable: false"), "{rendered}");
    }

    #[test]
    fn entry_render_mentions_provenance() {
        let rendered = render();
        assert!(rendered.contains("AUTO-GENERATED"), "{rendered}");
        assert!(rendered.contains("tm generate capabilities"), "{rendered}");
    }

    #[test]
    fn entry_render_is_deterministic() {
        assert_eq!(render(), render());
    }
}
