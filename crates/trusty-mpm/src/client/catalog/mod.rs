//! The command catalog — single source of truth for the verb surface.
//!
//! Why: before this module, three things described the same verb set and could
//! silently drift: the `/help` body (hand-written in `command.rs`), the typed
//! [`TrustyCommand`] enum, and the SM prompt's `SM_TOOLS.md` prose. The catalog
//! collapses them into one structured registry. Every adapter renders its help
//! from the same [`CommandSpec`] list, and the SM prompt's tool surface is
//! rendered from the same registry, so a new verb is described in exactly one
//! place. This is the "catalog as single source of truth" principle of the
//! chat-core nucleus (see `docs/specs/chat-core.md`, SPEC-CHAT-CORE-01).
//! What: [`CommandSpec`] is one entry per operator verb — its canonical name,
//! aliases, argument list, one-line summary, and the [`SpecKind`] that ties it
//! to a [`TrustyCommand`] variant (or marks it SM-only). [`catalog`] returns the
//! full registry; [`help_text`] renders the `/help` body; [`render_sm_tools`]
//! renders the `SM_TOOLS.md` prompt section. A unit test asserts the committed
//! `SM_TOOLS.md` byte-matches [`render_sm_tools`] so the two can never diverge.
//! Test: `catalog_covers_every_command`, `help_text_lists_every_command`,
//! `sm_tools_md_matches_catalog_render` (the parity / single-source-of-truth
//! guard).

use std::sync::OnceLock;

mod sm_tools;

pub use sm_tools::{render_sm_tools, sm_ops_verbs, sm_session_verbs};

/// How a catalog entry relates to the typed [`TrustyCommand`] surface.
///
/// Why: most catalog verbs map onto a `TrustyCommand` variant an adapter can
/// dispatch, but a few SM prompt verbs (memory, goals) are SM-only — the SM
/// calls them as namespaced JSON-RPC methods, not through the chat-core
/// executor. Modeling that distinction keeps both renderings (operator `/help`
/// vs. SM prompt) drawn from one list without pretending every prompt verb is a
/// dispatchable command.
/// What: `Command` carries the canonical slash name an adapter parses;
/// `SmOnly` marks a prompt-surface verb with no chat-core dispatch.
/// Test: covered by `catalog_covers_every_command` and the SM-tools parity test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    /// A dispatchable operator command, keyed by its canonical slash name
    /// (without the leading `/`), e.g. `"sessions"`.
    Command,
    /// An SM-prompt-only verb (memory/goals) with no chat-core dispatch.
    SmOnly,
}

/// One catalog entry: a verb, its surface forms, and its one-line summary.
///
/// Why: a single structured description per verb lets every rendering (operator
/// `/help`, the SM prompt tool table) derive from the same data, so adding or
/// renaming a verb touches exactly one place.
/// What: the canonical `name`, any `aliases`, the human-readable `args` sketch,
/// a one-line `summary`, and the [`SpecKind`] that ties it to the typed command
/// surface. All fields are `'static` so the catalog is a compile-time constant
/// table with no allocation.
/// Test: `catalog_covers_every_command`, `sm_tools_md_matches_catalog_render`.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Canonical verb name. For [`SpecKind::Command`] this is the slash command
    /// (without `/`); for [`SpecKind::SmOnly`] it is the namespaced JSON-RPC
    /// method spelling (e.g. `memory.recall(query)`).
    pub name: &'static str,
    /// Alternate spellings the adapters accept for this verb (may be empty).
    pub aliases: &'static [&'static str],
    /// Human-readable argument sketch (e.g. `<id>`), empty when the verb takes
    /// no arguments.
    pub args: &'static str,
    /// One-line operator-facing summary.
    pub summary: &'static str,
    /// How the entry relates to the typed [`TrustyCommand`] surface.
    pub kind: SpecKind,
}

impl CommandSpec {
    /// The callable signature for the SM-prompt tool table.
    ///
    /// Why: the SM prompt renders each verb as a callable `name(args)` form (e.g.
    /// `sessions.launch(workdir, model?, prompt?, goal_id?)`), matching the
    /// hand-written `SM_TOOLS.md`. Both SM-only and command entries carry their
    /// args in the dedicated `args` field, so the signature is computed uniformly.
    /// What: renders `name(args)`; an empty `args` yields `name()`. Operator-facing
    /// angle brackets (`<id>`) are stripped so the prompt reads as a method call.
    /// Test: `sm_tools_md_matches_catalog_render`, `prompt_signature_renders_callable_verbs`.
    pub fn prompt_signature(&self) -> String {
        if self.args.is_empty() {
            format!("{}()", self.name)
        } else {
            let inner = self.args.replace(['<', '>'], "");
            format!("{}({inner})", self.name)
        }
    }
}

/// The operator-facing chat-core commands (every dispatchable verb).
///
/// Why: this is the list every adapter renders its `/help` from and the typed
/// dispatch the executor exhausts; it covers the legacy project-session/pairing
/// family AND the managed session-manager verb set.
/// What: ordered for a readable `/help`; each entry's `name` is the canonical
/// slash command the adapters parse.
/// Test: `catalog_covers_every_command`, `help_text_lists_every_command`.
const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "sessions",
        aliases: &[],
        args: "",
        summary: "list managed sessions",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "status",
        aliases: &[],
        args: "<id>",
        summary: "detailed session status",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "approve",
        aliases: &[],
        args: "<id>",
        summary: "approve a pending permission request",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "deny",
        aliases: &[],
        args: "<id>",
        summary: "deny a pending permission request",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "overseer",
        aliases: &[],
        args: "",
        summary: "show overseer status",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "tmux",
        aliases: &[],
        args: "",
        summary: "list all tmux sessions",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "projects",
        aliases: &[],
        args: "",
        summary: "discover projects from Claude Code config",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "discover",
        aliases: &[],
        args: "",
        summary: "auto-discover tmux sessions running Claude Code",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "adopt",
        aliases: &[],
        args: "<session>",
        summary: "adopt an external tmux session",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "config",
        aliases: &[],
        args: "<path>",
        summary: "analyze Claude Code config for a project",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "snapshot",
        aliases: &[],
        args: "<session>",
        summary: "capture a tmux pane",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "kill",
        aliases: &[],
        args: "<id>",
        summary: "kill a session",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "send",
        aliases: &[],
        args: "<session> <prompt>",
        summary: "send a prompt to a Claude Code session",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "connect",
        aliases: &[],
        args: "<path>",
        summary: "connect to or start a session (no deployment)",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "alerts",
        aliases: &[],
        args: "",
        summary: "show alert subscriptions",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "pair",
        aliases: &[],
        args: "<code>",
        summary: "pair this bot with your daemon",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "start",
        aliases: &[],
        args: "",
        summary: "pairing status and onboarding",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "doctor",
        aliases: &[],
        args: "",
        summary: "run full system diagnostic",
        kind: SpecKind::Command,
    },
    // ── Managed session-manager verbs ───────────────────────────────────────
    CommandSpec {
        name: "managed-new",
        aliases: &["spawn"],
        args: "<repo> <ref> <task>",
        summary: "spawn a managed session from a repo + ref + task",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-list",
        aliases: &[],
        args: "",
        summary: "list managed session-manager sessions",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-get",
        aliases: &[],
        args: "<id>",
        summary: "fetch one managed session's record",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-send",
        aliases: &[],
        args: "<id> <text>",
        summary: "inject text into a managed session's pane",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-answer",
        aliases: &[],
        args: "<id> <answer>",
        summary: "answer a managed session's pending decision",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-activity",
        aliases: &[],
        args: "<id>",
        summary: "inspect a managed session's activity",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-attach",
        aliases: &[],
        args: "<id>",
        summary: "show a managed session's tmux attach command",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-stop",
        aliases: &[],
        args: "<id>",
        summary: "stop a managed session's runtime (keeps the workspace)",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-resume",
        aliases: &[],
        args: "<id>",
        summary: "resume a stopped managed session",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-decommission",
        aliases: &[],
        args: "<id>",
        summary: "decommission a managed session (terminal; removes workspace)",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "managed-adopt",
        aliases: &[],
        args: "<tmux_name> <cwd>",
        summary: "adopt an existing tmux session into the managed store",
        kind: SpecKind::Command,
    },
    // ── Ops / diagnostics ───────────────────────────────────────────────────
    CommandSpec {
        name: "health",
        // `status` is intentionally NOT an alias — it is already the
        // project-session status verb, so reusing it would collide. `healthcheck`
        // and `ping` are the natural operator spellings.
        aliases: &["healthcheck", "ping"],
        args: "",
        summary: "report daemon health (reachability, catalog freshness, fleet)",
        kind: SpecKind::Command,
    },
    CommandSpec {
        name: "help",
        aliases: &[],
        args: "",
        summary: "this message",
        kind: SpecKind::Command,
    },
];

/// The full operator command catalog.
///
/// Why: adapters and tests need read access to the registry to render help,
/// build pickers, and assert coverage.
/// What: returns the static [`CommandSpec`] slice of every dispatchable command.
/// Test: `catalog_covers_every_command`.
pub fn catalog() -> &'static [CommandSpec] {
    COMMANDS
}

/// Render the `/help` body from the command catalog.
///
/// Why: the help text is derived from the same registry the executor dispatches,
/// so it can never list a command the executor cannot run, nor omit one.
/// What: one line per [`SpecKind::Command`] entry, `"/name <args> — summary"`,
/// computed once and cached for the `&'static str` callers expect.
/// Test: `help_text_lists_every_command`.
pub fn help_text() -> &'static str {
    static HELP: OnceLock<String> = OnceLock::new();
    HELP.get_or_init(|| {
        let mut out = String::from("trusty-mpm bot commands:\n");
        let lines: Vec<String> = COMMANDS
            .iter()
            .filter(|c| c.kind == SpecKind::Command)
            .map(|c| {
                if c.args.is_empty() {
                    format!("/{} — {}", c.name, c.summary)
                } else {
                    format!("/{} {} — {}", c.name, c.args, c.summary)
                }
            })
            .collect();
        out.push_str(&lines.join("\n"));
        out
    })
    .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_every_command() {
        // The catalog must contain every operator-facing slash command, keyed by
        // its canonical name without the leading slash.
        let names: Vec<&str> = catalog()
            .iter()
            .filter(|c| c.kind == SpecKind::Command)
            .map(|c| c.name)
            .collect();
        for expected in [
            "sessions",
            "status",
            "approve",
            "deny",
            "overseer",
            "tmux",
            "projects",
            "discover",
            "adopt",
            "config",
            "snapshot",
            "kill",
            "send",
            "connect",
            "alerts",
            "pair",
            "start",
            "doctor",
            "managed-new",
            "managed-list",
            "managed-get",
            "managed-send",
            "managed-answer",
            "managed-activity",
            "managed-attach",
            "managed-stop",
            "managed-resume",
            "managed-decommission",
            "managed-adopt",
            "health",
            "help",
        ] {
            assert!(names.contains(&expected), "catalog missing `{expected}`");
        }
    }

    #[test]
    fn health_verb_has_expected_aliases_and_no_status_collision() {
        // `health` carries the operator-friendly spellings but NEVER `status`,
        // which is already the project-session status verb (a collision).
        let health = catalog()
            .iter()
            .find(|c| c.name == "health")
            .expect("catalog has a health verb");
        assert!(health.aliases.contains(&"healthcheck"));
        assert!(health.aliases.contains(&"ping"));
        assert!(
            !health.aliases.contains(&"status"),
            "`status` must not alias health — it is the session status verb"
        );
        assert_eq!(health.kind, SpecKind::Command);
    }

    #[test]
    fn catalog_has_no_duplicate_command_names_or_aliases() {
        // Every dispatchable verb's canonical name AND each alias must be unique
        // across the catalog, so a parsed token resolves to exactly one verb.
        use std::collections::HashSet;
        let mut seen: HashSet<&str> = HashSet::new();
        for spec in catalog().iter().filter(|c| c.kind == SpecKind::Command) {
            assert!(
                seen.insert(spec.name),
                "duplicate verb name `{}`",
                spec.name
            );
            for alias in spec.aliases {
                assert!(seen.insert(alias), "duplicate alias `{alias}`");
            }
        }
    }

    #[test]
    fn help_text_lists_every_command() {
        let text = help_text();
        for cmd in [
            "/sessions",
            "/status",
            "/approve",
            "/deny",
            "/overseer",
            "/tmux",
            "/projects",
            "/discover",
            "/adopt",
            "/config",
            "/snapshot",
            "/kill",
            "/send",
            "/connect",
            "/alerts",
            "/pair",
            "/start",
            "/doctor",
            "/managed-new",
            "/managed-list",
            "/managed-get",
            "/managed-send",
            "/managed-answer",
            "/managed-activity",
            "/managed-attach",
            "/managed-stop",
            "/managed-resume",
            "/managed-decommission",
            "/managed-adopt",
            "/health",
            "/help",
        ] {
            assert!(text.contains(cmd), "help text missing {cmd}");
        }
    }
}
