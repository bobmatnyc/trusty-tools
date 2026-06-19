//! SM-prompt tool-surface rendering, derived from the command catalog.
//!
//! Why: the SM's `SM_TOOLS.md` prompt asset and the chat-core command surface
//! describe the same verbs and must never drift. Keeping the SM verb tables and
//! the `render_sm_tools` renderer here (a sibling of the catalog types) makes the
//! catalog the single source of truth for the prompt while keeping each file
//! under the 500-SLOC cap. A committed `SM_TOOLS.md` is asserted byte-identical
//! to [`render_sm_tools`] by [`sm_tools_md_matches_catalog_render`].
//! What: the three SM verb tables (session control, memory, goals) as
//! [`CommandSpec`] slices, and [`render_sm_tools`] which emits the full markdown
//! document — fixed prose plus the three tables.
//! Test: `sm_tools_md_matches_catalog_render`, `prompt_signature_renders_callable_verbs`.

use super::{CommandSpec, SpecKind};

/// The session-control verbs the SM drives its fleet through (prompt table).
///
/// Why: these are the catalog rows the `SM_TOOLS.md` "Session control" table
/// renders AND the managed verbs the chat-core executor dispatches; pinning them
/// in one slice keeps the prompt and the wire surface aligned (the SM's
/// `sessions.launch` is the executor's [`crate::TrustyCommand::ManagedNew`]).
/// What: ordered to match the bundled `SM_TOOLS.md` session-control table.
/// Test: `sm_tools_md_matches_catalog_render`.
const SM_SESSION_VERBS: &[CommandSpec] = &[
    CommandSpec {
        name: "sessions.launch",
        aliases: &[],
        args: "workdir, model?, prompt?, goal_id?",
        summary: "Launch a new t-mpm session scoped to a task; links to a goal when `goal_id` is given",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.list",
        aliases: &[],
        args: "",
        summary: "List the current fleet of sessions",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.get",
        aliases: &[],
        args: "session_id",
        summary: "Fetch a session's state, captured output, and events",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.send",
        aliases: &[],
        args: "session_id, text",
        summary: "Send a command / answer a decision prompt to a session",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.stop",
        aliases: &[],
        args: "session_id",
        summary: "Gracefully stop a session",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.resume",
        aliases: &[],
        args: "session_id",
        summary: "Resume a paused or stopped session",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "sessions.kill",
        aliases: &[],
        args: "session_id",
        summary: "Force-stop / reap an unresponsive or dead session",
        kind: SpecKind::SmOnly,
    },
];

/// The SM memory verbs (prompt table only).
const SM_MEMORY_VERBS: &[CommandSpec] = &[
    CommandSpec {
        name: "memory.recall",
        aliases: &[],
        args: "query",
        summary: "Recall relevant context (prior goals, outcomes, decisions) for the current intent",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "memory.remember",
        aliases: &[],
        args: "text",
        summary: "Store a durable fact: goal records, session outcomes, cross-session knowledge",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "memory.note",
        aliases: &[],
        args: "text",
        summary: "Record a decision or blocker as it happens (\"chose Bedrock for cost\", \"split goal X into 3 sessions\")",
        kind: SpecKind::SmOnly,
    },
];

/// The SM goal verbs (prompt table only).
const SM_GOAL_VERBS: &[CommandSpec] = &[
    CommandSpec {
        name: "goals.list",
        aliases: &[],
        args: "status?",
        summary: "List goals, optionally filtered by status",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "goals.create",
        aliases: &[],
        args: "description, acceptance?",
        summary: "Create a goal from operator intent with testable acceptance criteria",
        kind: SpecKind::SmOnly,
    },
    CommandSpec {
        name: "goals.update",
        aliases: &[],
        args: "id, status?, progress?, note?",
        summary: "Update status / progress / append a note as sessions are verified",
        kind: SpecKind::SmOnly,
    },
];

/// Render one prompt-table row for a [`CommandSpec`].
fn render_table_row(spec: &CommandSpec) -> String {
    format!("| `{}` | {} |", spec.prompt_signature(), spec.summary)
}

/// Render the `SM_TOOLS.md` prompt section from the catalog.
///
/// Why: `SM_TOOLS.md` is the SM's tool-surface prompt; deriving its three verb
/// tables (session control, memory, goals) from the same catalog the executor
/// dispatches makes the catalog the single source of truth. A committed
/// `SM_TOOLS.md` is kept byte-identical to this output by
/// `sm_tools_md_matches_catalog_render`, so the prompt and the code can never
/// silently drift.
/// What: emits the full markdown document — the fixed prose plus the three
/// tables rendered from [`SM_SESSION_VERBS`], [`SM_MEMORY_VERBS`], and
/// [`SM_GOAL_VERBS`]. The trailing newline matches the committed file.
/// Test: `sm_tools_md_matches_catalog_render`.
pub fn render_sm_tools() -> String {
    let mut out = String::new();
    out.push_str("# SM Tools -- the verbs you may call\n\n");
    out.push_str(
        "These are the only verb surfaces available to you. They map onto the\n\
         session-manager control surface, the `session-manager` memory palace, and the\n\
         goal model. Anything that is *not* one of these verbs -- editing files, reading\n\
         project source, running builds/tests -- is a prohibition (SP1-SP5): launch a\n\
         session for it.\n\n",
    );

    out.push_str("## Session control (your hands)\n\n");
    out.push_str(
        "You drive sessions exclusively through these verbs; you never bypass the control\n\
         surface. Observing **panes** is allowed; reading project **source** is not.\n\n",
    );
    out.push_str(
        "All verbs are **namespaced** JSON-RPC methods (`sessions.*`, `goals.*`,\n\
         `memory.*`) -- the exact wire surface the SM-STDIO adapter exposes. Always call\n\
         them by their fully namespaced name so the prompt and the wire never diverge.\n\n",
    );
    out.push_str("| Verb | Purpose |\n|------|---------|\n");
    for spec in SM_SESSION_VERBS {
        out.push_str(&render_table_row(spec));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(
        "Use `sessions.get` to OBSERVE (phase 4) and to gather VERIFY evidence (phase 5).\n\
         Use `sessions.send` to answer session decision prompts. Use\n\
         `sessions.stop`/`sessions.kill` when a task is complete, abandoned, or wedged.\n\n",
    );

    out.push_str("## Memory (your durable knowledge)\n\n");
    out.push_str(
        "Scoped by default to the **`session-manager` palace** -- your own dedicated,\n\
         cross-session store, distinct from project and personal palaces. You read from\n\
         it at INTAKE and write to it at REPORT & PERSIST.\n\n",
    );
    out.push_str("| Verb | Purpose |\n|------|---------|\n");
    for spec in SM_MEMORY_VERBS {
        out.push_str(&render_table_row(spec));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(
        "You may recall from granted read-only palaces, but you **only ever write to the\n\
         `session-manager` palace** -- never to project or personal palaces.\n\n",
    );

    out.push_str("## Goals (your tracking model)\n\n");
    out.push_str(
        "A goal maps operator intent to one-or-many launched sessions (fan-out). Each\n\
         session belongs to exactly one goal. You maintain the `goal_id <-> session_id`\n\
         index.\n\n",
    );
    out.push_str("| Verb | Purpose |\n|------|---------|\n");
    for spec in SM_GOAL_VERBS {
        out.push_str(&render_table_row(spec));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(
        "A goal closes (`status = Done`) only when **every linked task is verified and\n\
         its acceptance criteria are met** -- gated by the verification gate in\n\
         SM_WORKFLOW. Otherwise it is `Blocked` or `Abandoned` with a recorded reason.\n\n",
    );

    out.push_str("## Tone & output\n\n");
    out.push_str(
        "Concise, operator-facing, status-first. Lead with what changed and what is\n\
         blocked. When summarizing the fleet, one line per session: `ID | crisp status`.\n\
         Surface decisions the operator must make. Never pad.\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sm_tools_md_matches_catalog_render() {
        // The committed SM_TOOLS.md prompt asset MUST be byte-identical to the
        // catalog's render. This is the single-source-of-truth guard: if a verb
        // changes in the catalog without regenerating the asset (or vice versa),
        // this test fails, so the prompt and the code can never silently drift.
        let committed = include_str!("../../assets/sm_instructions/SM_TOOLS.md");
        let rendered = render_sm_tools();
        assert_eq!(
            rendered, committed,
            "SM_TOOLS.md is out of sync with catalog::render_sm_tools(); \
             regenerate it (the catalog is authoritative)"
        );
    }

    #[test]
    fn prompt_signature_renders_callable_verbs() {
        // Every catalog row renders `name(args)`; an SM verb keeps its namespaced
        // spelling, args inlined.
        let sm = SM_SESSION_VERBS
            .iter()
            .find(|c| c.name == "sessions.launch")
            .unwrap();
        assert_eq!(
            sm.prompt_signature(),
            "sessions.launch(workdir, model?, prompt?, goal_id?)"
        );
    }
}
