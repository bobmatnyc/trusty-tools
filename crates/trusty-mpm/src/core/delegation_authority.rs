//! Delegation authority — scan deployed agents and render a routing section.
//!
//! Why: the orchestrating Claude Code instance needs to know which agents are
//! deployed and what each one handles so it can route work correctly; that
//! list is dynamic (it depends on what was deployed) and must be regenerated
//! at every session start.
//! What: [`scan_agents`] reads every `.md` file in an agents directory, parses
//! its frontmatter, and returns one [`AgentSummary`] per deployable (non-base)
//! agent; [`generate_authority`] folds those summaries into a Markdown section
//! injected into the session launch instructions;
//! [`resolve_roster`] is THE roster resolver every consumer shares — the PM
//! prompt's delegation section, the `tm session start` count, and `tm doctor`
//! (#4588); [`deployed_roster_section`] renders what it returns (#4069).
//! Test: `delegation_authority_tests.rs` covers scanning, foundation-file
//! exclusion and its near misses, the `name:`-required admission rule (#4711),
//! the empty directory, and both render branches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::frontmatter::parse_kv_line;

/// A deployed agent as advertised to the orchestrating instance.
///
/// Why: the delegation authority section needs a small, render-ready view of
/// each agent — name, role, what it handles, its foundation chain, and a model
/// hint — without exposing the full composed agent body.
/// What: the display fields parsed from a composed agent's frontmatter plus the
/// resolved `extends` chain (base-first, ending in the agent itself).
/// Test: exercised by every `scan_*` and `generate_*` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSummary {
    /// Agent name (frontmatter `name`, falling back to the file stem).
    pub name: String,
    /// Agent role (frontmatter `role`, falling back to `name`).
    pub role: String,
    /// One-line description of what the agent handles, if declared.
    pub description: Option<String>,
    /// Model hint (frontmatter `model`), if declared.
    pub model: Option<String>,
    /// Resolved inheritance chain, base-first, e.g.
    /// `["base-agent", "base-engineer", "engineer"]`.
    pub extends_chain: Vec<String>,
}

/// File name of the deploy manifest, excluded from agent scans.
const MANIFEST_FILE: &str = "manifest.json";

/// The minimal frontmatter fields a composed agent advertises.
///
/// Why: scanning only needs a handful of YAML keys; a tiny struct avoids a
/// full YAML dependency and keeps parsing explicit.
/// What: the display fields plus `extends` (composed agents normally carry no
/// `extends`, but it is parsed so a hand-written source dir still resolves).
/// Test: exercised indirectly by every `scan_*` test.
#[derive(Debug, Default)]
struct AgentFrontmatter {
    name: Option<String>,
    role: Option<String>,
    description: Option<String>,
    model: Option<String>,
    extends: Option<String>,
}

/// Whether a parsed value is a YAML block-scalar header rather than a value.
///
/// Why: `description: >` means "the value is on the following indented lines",
/// not "the value is `>`". Five bundled writing agents (`copyeditor`,
/// `pangram-editor`, `proofreader`, `writer`, `writing-critic`) author their
/// descriptions that way, and taking the marker literally rendered each one into
/// the PM prompt as a bare `>`.
/// What: matches `>` or `|` optionally followed by a chomping indicator (`-`,
/// `+`) and/or an explicit indentation digit, in either order — the full set of
/// YAML block-scalar headers.
/// Test: `block_scalar_description_is_folded`,
/// `literal_block_scalar_description_keeps_its_line_breaks`.
fn is_block_scalar_header(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['>', '|']) else {
        return false;
    };
    rest.chars().all(|c| matches!(c, '-' | '+' | '1'..='9'))
}

/// Consume a block scalar's body from the frontmatter line stream.
///
/// Why: the body is whatever follows the header, indented, up to the first line
/// that is not — which is how the closing `---` (column 0) terminates it without
/// a special case.
/// What: takes indented and blank lines from `lines`, then joins them. A folded
/// header (`>`) joins the lines with a space and turns a blank line into a line
/// break, per YAML folding; a literal header (`|`) preserves every line break.
/// Test: `block_scalar_description_is_folded`,
/// `literal_block_scalar_description_keeps_its_line_breaks`.
fn read_block_scalar(header: &str, lines: &mut std::iter::Peekable<std::str::Lines<'_>>) -> String {
    let folded = header.starts_with('>');
    let mut body: Vec<String> = Vec::new();
    while let Some(next) = lines.peek() {
        if !next.trim().is_empty() && !next.starts_with([' ', '\t']) {
            break;
        }
        body.push(lines.next().unwrap_or_default().trim().to_string());
    }
    while body.last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    if !folded {
        return body.join("\n");
    }
    // Folded: run together, but a blank line is a deliberate paragraph break.
    let mut out = String::new();
    for line in body {
        if line.is_empty() {
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(&line);
        }
    }
    out
}

/// Parse the leading `---` frontmatter block of a Markdown document.
///
/// Why: composed agents store their metadata in a YAML-ish frontmatter block;
/// the scanner reads just the keys it needs without a YAML library.
/// What: if the document opens with a `---` line, collects `key: value` pairs
/// until the closing `---`; quotes are stripped and keys lower-cased. A value
/// that is a block-scalar header (`>`, `|`, with any chomping indicator) takes
/// the following indented lines as its value via [`read_block_scalar`]. A
/// document with no frontmatter yields an all-`None` result.
/// Test: `scan_finds_agents` (frontmatter present), `scan_handles_no_frontmatter`,
/// `block_scalar_description_is_folded`.
fn parse_frontmatter(raw: &str) -> AgentFrontmatter {
    let trimmed = raw.trim_start_matches(['\u{feff}']);
    let mut lines = trimmed.lines().peekable();

    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return AgentFrontmatter::default(),
    }

    let mut fm = AgentFrontmatter::default();
    while let Some(line) = lines.next() {
        if line.trim() == "---" {
            break;
        }
        // Use the shared parser so colon-containing values (URLs, timestamps,
        // model ids) are preserved rather than silently truncated.
        let Some((key, value)) = parse_kv_line(line) else {
            continue;
        };
        let value = if is_block_scalar_header(&value) {
            read_block_scalar(&value, &mut lines)
        } else {
            value
        };
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "name" => fm.name = Some(value),
            "role" => fm.role = Some(value),
            "description" => fm.description = Some(value),
            "model" => fm.model = Some(value),
            "extends" => fm.extends = Some(value),
            _ => {}
        }
    }
    fm
}

/// Whether a file stem names a foundation (non-delegatable) template.
///
/// Why (#4589): this is the ONLY rule that removes an agent from the roster, so
/// it must be narrow and it must exist in exactly one place. The `-` is the
/// whole point: `starts_with("base")` silently deletes `baseline-analyzer`,
/// `base64-decoder`, `basecamp-sync` and `based-reviewer` — ordinary names in
/// the project and `~/.claude/agents` tiers tm does not author, which is
/// precisely the failure mode the frontmatter `role:` rule was removed for.
/// Sharing the predicate with the asset-level gate keeps the shipped files and
/// the scanner from drifting apart the way the roster and its count did.
/// What: case-insensitive `base-` prefix on the file stem, OR the bare stem
/// `base` — so every bundled `BASE-*.md` matches, a hand-placed `base.md`
/// matches, and nothing else plausibly does.
/// Test: `scan_excludes_base_agents`,
/// `near_miss_base_names_are_not_treated_as_foundation_files`,
/// `bare_base_md_is_excluded_from_the_roster`,
/// `base_prefixed_near_misses_survive_the_bare_base_rule`.
pub(crate) fn is_foundation_file(stem: &str) -> bool {
    // #4711: `base-` alone missed the hyphen-less `base.md`, whose body is a
    // "Base QA Instructions… Appended to all QA agents" prompt fragment. That
    // is exactly the shape a foundation file takes, so it must be excluded on
    // the same rule. Matching the WHOLE stem (not a `base` prefix) keeps the
    // #4589 near misses — `baseline-analyzer`, `base64-decoder` — in the roster.
    let lower = stem.to_ascii_lowercase();
    lower == "base" || lower.starts_with("base-")
}

/// Scan `agents_dir` and return summaries for all non-base agents.
///
/// Why: the orchestrating CC instance needs to know which agents exist
/// and what they handle so it can route work correctly.
///
/// What: reads all .md files in agents_dir, parses frontmatter (name,
/// role, description, model, extends), excludes BASE-* files, files with no
/// `name:` frontmatter, and the manifest file, returns one AgentSummary per
/// deployable agent.
///
/// Two rules remove a file from the roster, and only two:
/// 1. [`is_foundation_file`] — the `BASE-*` / bare `base` FILE NAME convention.
/// 2. No frontmatter `name:` (#4711) — Claude Code dispatches by `name:`, so a
///    file lacking one is a prompt fragment that could never be delegated to.
///    The live case was `~/.claude/agents/base.md`, a "Base QA Instructions…
///    Appended to all QA agents" fragment that the stem fallback turned into a
///    delegatable agent called `base`.
///
/// Foundation templates are identified by [`is_foundation_file`] — the `BASE-*`
/// FILE NAME convention alone (#4589). An earlier revision also dropped any
/// agent whose frontmatter
/// `role:` began with `base`, which silently deleted three real, dispatchable
/// agents — `memory-manager`, `mpm-agent-manager`, `mpm-skills-manager` — from
/// every roster. `role:` is the wrong signal for the job: the roster is a union
/// over three tiers (see [`deployed_agent_dirs`]) and tm authors the frontmatter
/// in exactly one of them, so no asset edit can stop the same rule from eating
/// an operator's own agent in `<project>/.claude/agents` or `~/.claude/agents`.
/// The file-name convention is tm's own, applies to the files tm actually
/// ships, and is checked before the file is even read.
///
/// Test: `scan_finds_agents`, `scan_excludes_base_agents`,
/// `agent_with_a_base_role_but_no_base_filename_stays_in_the_roster`,
/// `bare_base_md_is_excluded_from_the_roster`,
/// `file_without_name_frontmatter_is_excluded_from_the_roster`,
/// `scan_empty_dir`
pub fn scan_agents(agents_dir: &Path) -> Vec<AgentSummary> {
    scan_agents_reporting(agents_dir).agents
}

/// A roster scan's full result: what was found AND what was lost finding it.
///
/// Why (#5544): every roster scan is fail-open — an unreadable tier and an
/// unreadable agent file both degrade to "this file contributes nothing", which
/// is byte-identical to "the tier legitimately holds no agent". A caller that
/// only receives `Vec<AgentSummary>` therefore cannot tell a complete roster
/// from a truncated one, and a PM handed the truncated one routes around agents
/// that exist. Carrying the losses alongside the finds is what makes the
/// difference expressible at all.
/// What: `agents` is the same list [`scan_agents`] has always returned;
/// `unreadable` names every path that was present but could not be read — an
/// individual agent file, or the tier directory itself when enumeration failed
/// for any reason other than "not found".
/// Test: `an_unreadable_agent_file_is_reported_not_silently_dropped`,
/// `an_unreadable_tier_directory_is_reported_not_silently_dropped`.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RosterScan {
    /// Agents successfully parsed, name-sorted.
    pub agents: Vec<AgentSummary>,
    /// Paths that exist but could not be read, and so are MISSING from `agents`.
    pub unreadable: Vec<PathBuf>,
}

/// [`scan_agents`], additionally reporting what the scan could not read.
///
/// Why: see [`RosterScan`] — this is the variant that keeps a truncated roster
/// distinguishable from a short one.
/// What: identical scanning and admission rules; a non-`NotFound` `read_dir`
/// failure records the DIRECTORY, and a per-file read failure records the FILE,
/// each at `error!` rather than being dropped in silence.
/// Test: `an_unreadable_agent_file_is_reported_not_silently_dropped`,
/// `an_unreadable_tier_directory_is_reported_not_silently_dropped`.
pub fn scan_agents_reporting(agents_dir: &Path) -> RosterScan {
    // An ABSENT tier is normal (not every launch mode populates every tier) and
    // stays silent. Any OTHER enumeration failure — permissions, a bad mount, an
    // I/O fault — is NOT normal and must never be indistinguishable from "empty"
    // (#4069 review): a tier that fails to enumerate silently shrinks the roster,
    // and if every tier fails the prompt reverts to the stale bundled asset with
    // no alarm anywhere. Fail open (a launch must not be blocked by one bad
    // directory) but never fail quiet.
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return RosterScan::default(),
        Err(err) => {
            // #5544: a warn alone left the loss invisible to the composed prompt.
            tracing::error!(
                dir = %agents_dir.display(),
                %err,
                "agent tier could not be enumerated; the delegation roster is incomplete"
            );
            return RosterScan {
                agents: Vec::new(),
                unreadable: vec![agents_dir.to_path_buf()],
            };
        }
    };

    let mut unreadable: Vec<PathBuf> = Vec::new();
    let mut summaries: Vec<AgentSummary> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if file_name.eq_ignore_ascii_case(MANIFEST_FILE) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Exclude BASE-* source/foundation files by file name, before any read.
        // The trailing `-` is load-bearing (#4589 review): a bare `base` prefix
        // also eats `baseline-analyzer`, `base64-decoder`, `basecamp-sync` and
        // `based-reviewer` — plausible agent names in tiers tm does not author,
        // which is the exact failure this rule replaced, not a fix for it.
        if is_foundation_file(stem) {
            // A silent deletion is what made #4589 undiagnosable. An operator
            // whose `base-…` agent vanished has no other thread to pull.
            tracing::debug!(
                file = %file_name,
                dir = %agents_dir.display(),
                "excluded from the delegation roster as a BASE-* foundation template"
            );
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        // #5544: this was a bare `else { continue }` — a readable-but-failing
        // agent file left the roster one entry short with no signal anywhere.
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                tracing::error!(
                    file = %path.display(),
                    %err,
                    "agent file could not be read; it is MISSING from the delegation roster"
                );
                unreadable.push(path.clone());
                continue;
            }
        };
        let fm = parse_frontmatter(&raw);
        // #4711: a roster entry must be DISPATCHABLE, and Claude Code dispatches
        // a subagent by its frontmatter `name:`. A file without one is a prompt
        // fragment, not an agent; the old file-stem fallback minted a delegation
        // target the harness cannot actually resolve. This is the durable half of
        // the fix — `is_foundation_file` only catches fragments that happen to be
        // named like one.
        let Some(name) = fm.name.clone() else {
            // Never silent: an operator whose file vanished from the roster needs
            // a thread to pull, exactly as for the foundation-file exclusion.
            tracing::debug!(
                file = %file_name,
                dir = %agents_dir.display(),
                "excluded from the delegation roster: no `name:` frontmatter, so it is \
                 not a dispatchable agent"
            );
            continue;
        };
        let role = fm.role.clone().unwrap_or_else(|| name.clone());
        let extends_chain = build_extends_chain(fm.extends.as_deref(), &name);

        summaries.push(AgentSummary {
            name,
            role,
            description: fm.description,
            model: fm.model,
            extends_chain,
        });
    }

    // Deterministic order so the rendered section is stable across runs.
    summaries.sort_by_key(|a| a.name.clone());
    unreadable.sort();
    RosterScan {
        agents: summaries,
        unreadable,
    }
}

/// Build a base-first inheritance chain from a single `extends` parent.
///
/// Why: composed agents flatten their inheritance into one file and normally
/// carry no `extends`; when one is present we still record the immediate
/// parent so the rendered "Foundation" line is informative.
/// What: returns `[parent, name]` when an `extends` parent exists, otherwise
/// just `[name]`.
/// Test: `scan_finds_agents` asserts the single-element chain.
fn build_extends_chain(extends: Option<&str>, name: &str) -> Vec<String> {
    match extends {
        Some(parent) if !parent.is_empty() => {
            vec![parent.to_string(), name.to_string()]
        }
        _ => vec![name.to_string()],
    }
}

/// Generate the delegation authority Markdown section from summaries.
///
/// Why: injected into session launch instructions so the orchestrating
/// instance knows its delegation options.
///
/// What: produces a Markdown section listing each agent with name,
/// description, extends chain, and model hint.
///
/// Zero-information lines are omitted (#4069 review): `Role` is skipped when it
/// merely repeats the `###` heading (it falls back to `name` when unset, true
/// for ~14 of 40 deployed agents) and `Foundation` is skipped for a
/// single-element chain, which renders as the agent citing itself as its own
/// foundation (true for every composed agent, since composition flattens
/// `extends`). This section is re-emitted into EVERY PM session, so ~2 KB of
/// per-session noise is worth deleting.
///
/// `Handles` is omitted for the same reason, one level up: its text is the
/// agent's frontmatter `description`, which is byte-identical to the description
/// the harness already publishes for that agent in its own Agent-type catalog.
/// Re-emitting it made the roster a second copy of a list the session already
/// has. What the harness does NOT publish is `Role` and `Model`, so those are
/// the roster's whole contribution and they stay. `description` is still parsed
/// into [`AgentSummary`] — it is public API and other consumers read it.
///
/// Test: `generate_authority_nonempty`, `generate_authority_empty`,
/// `generate_authority_omits_self_referential_lines`,
/// `generate_authority_omits_the_harness_supplied_description`
pub fn generate_authority(agents: &[AgentSummary]) -> String {
    generate_authority_reporting(agents, &[])
}

/// The banner [`generate_authority_reporting`] prepends when the scan lost
/// entries. Asserted on by name so the wording can change without the gate
/// silently stopping to check anything.
pub const ROSTER_INCOMPLETE_MARKER: &str = "ROSTER INCOMPLETE";

/// [`generate_authority`], rendering an explicit incompleteness banner when the
/// scan could not read one or more paths.
///
/// Why (#5544): the composed prompt is the artifact whose truncation is
/// invisible — a PM handed a short roster reads it as the complete set and
/// routes around agents that exist. A `tracing::error!` records the loss where
/// nobody reading the prompt will see it, so the loss has to appear IN the
/// prompt. This is deliberately still fail-open: a launch must not be blocked by
/// one bad directory, but it must never claim a roster it does not have.
/// What: prepends a banner naming every unreadable path above the agent list,
/// then renders exactly as [`generate_authority`]. With `unreadable` empty the
/// output is byte-identical to the previous behaviour.
/// Test: `an_unreadable_agent_file_is_reported_not_silently_dropped`,
/// `an_unreadable_tier_directory_is_reported_not_silently_dropped`,
/// `generate_authority_is_unchanged_when_nothing_was_lost`.
pub fn generate_authority_reporting(agents: &[AgentSummary], unreadable: &[PathBuf]) -> String {
    let mut out = String::from("## Delegation Authority\n\n");

    if !unreadable.is_empty() {
        let paths: Vec<String> = unreadable
            .iter()
            .map(|p| format!("`{}`", p.display()))
            .collect();
        out.push_str(&format!(
            "> **{marker} ({count} path(s) unreadable).** {paths} could not be read, so any \
             agent they hold is ABSENT from the list below. Treat a missing agent as \
             UNAVAILABLE, not nonexistent — run `tm doctor` before concluding it does not \
             exist. See issue #5544.\n\n",
            marker = ROSTER_INCOMPLETE_MARKER,
            count = unreadable.len(),
            paths = paths.join(", "),
        ));
    }

    if agents.is_empty() {
        out.push_str(
            "No delegatable agents are currently available. Handle all work \
             directly until agents are deployed.\n",
        );
        return out;
    }

    out.push_str(
        "The following agents are available for delegation. Route work to the\n\
         appropriate agent based on task type.\n\n",
    );

    for agent in agents {
        out.push_str(&format!("### {}\n", agent.name));
        if agent.role != agent.name {
            out.push_str(&format!("- **Role:** {}\n", agent.role));
        }
        if agent.extends_chain.len() > 1 {
            out.push_str(&format!(
                "- **Foundation:** {}\n",
                agent.extends_chain.join(" → ")
            ));
        }
        if let Some(model) = &agent.model {
            out.push_str(&format!("- **Model:** {model}\n"));
        }
        out.push('\n');
    }

    out
}

/// Agent directories a launched session can load composed agents from,
/// highest precedence first.
///
/// Why (#4069): the roster the PM must route to is whatever Claude Code will
/// actually load, and that is a union of tiers rather than a single directory.
/// Since #4409 every BUNDLED agent deploys into exactly one of them — the
/// tm-owned `CLAUDE_CONFIG_DIR/agents` — while `<project>/.claude/agents` holds
/// only agents the operator hand-placed (and, in future, project-custom
/// trusty-built agents), and `~/.claude/agents` holds whatever the operator's
/// own generic Claude Code install carries, which tm never writes to. All three
/// are still scanned because a session can legitimately resolve an agent from
/// any of them, and the project tier still WINS on a name collision. Prompt
/// composition only ever receives a `project_dir`, so it resolves all three
/// tiers here rather than depending on a launch-mode value it cannot see.
/// What: returns `<project>/.claude/agents`, then the active managed
/// `CLAUDE_CONFIG_DIR/agents` (the `CLAUDE_CONFIG_DIR` env var when set,
/// otherwise [`managed_claude_config_dir`]), then
/// `FrameworkPaths::default().claude_agents_dir()`. Paths are returned whether
/// or not they exist; [`scan_agents`] treats an unreadable directory as empty.
/// Test: `deployed_agent_dirs_puts_project_tier_first`.
pub fn deployed_agent_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let managed = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(crate::core::trusty_tools_config::managed_claude_config_dir);
    deployed_agent_dirs_from(
        project_dir,
        managed.as_deref(),
        &crate::core::paths::FrameworkPaths::default().claude_agents_dir(),
    )
}

/// The tier ORDER behind [`deployed_agent_dirs`], with every environment
/// lookup hoisted out.
///
/// Why (#4946): the compiled prompt used to state a precedence no code read
/// (`~/.trusty-mpm/agents/`), because the real order lived only here and was
/// restated by hand elsewhere. The `tm-capabilities` skill now renders that
/// order instead of restating it, and the renderer must be reproducible — it
/// cannot call [`deployed_agent_dirs`], which reads `CLAUDE_CONFIG_DIR` and
/// the caller's home directory and so differs per machine. Splitting the pure
/// ordering out means the generated documentation and the runtime resolution
/// are the same three lines of code: reorder the tiers and the committed skill
/// drifts, failing `scripts/check_capabilities.sh`.
/// What: `<project>/.claude/agents`, then `<managed_config_dir>/agents` when
/// one is supplied, then `framework_claude_agents` verbatim. No I/O, no env.
/// Test: `deployed_agent_dirs_from_is_pure_and_ordered`,
/// `deployed_agent_dirs_from_skips_absent_managed_tier`.
pub fn deployed_agent_dirs_from(
    project_dir: &Path,
    managed_config_dir: Option<&Path>,
    framework_claude_agents: &Path,
) -> Vec<PathBuf> {
    let mut dirs = vec![project_dir.join(".claude").join("agents")];
    if let Some(managed) = managed_config_dir {
        dirs.push(managed.join("agents"));
    }
    dirs.push(framework_claude_agents.to_path_buf());
    dirs
}

/// Merge the agents found across `dirs` into one roster, earlier dirs winning.
///
/// Why: the tiers returned by [`deployed_agent_dirs`] overlap — the same agent
/// is normally deployed into more than one of them — so a naive concatenation
/// would advertise duplicates. Claude Code resolves a same-named agent by tier
/// precedence, and the rendered roster must describe the copy that actually
/// wins.
/// What: scans each directory in order and keeps the FIRST summary seen per
/// agent name, returning them name-sorted for a stable prompt. The dedup key is
/// LOWER-CASED — agent names are case-insensitive to Claude Code's dispatcher,
/// so `QA` in one tier and `qa` in another are one agent, not two.
/// Test: `roster_from_dirs_dedupes_with_first_dir_winning`,
/// `roster_from_dirs_dedup_is_case_insensitive`.
pub fn roster_from_dirs(dirs: &[PathBuf]) -> Vec<AgentSummary> {
    roster_from_dirs_reporting(dirs).agents
}

/// [`roster_from_dirs`], additionally reporting what the union could not read.
///
/// Why: see [`RosterScan`]. The union is where a per-tier loss stops being
/// attributable — one tier failing shrinks the merged roster by however many
/// agents only that tier carried, and nothing downstream can tell.
/// What: same first-tier-wins, case-insensitive dedup; concatenates every
/// tier's `unreadable` paths.
/// Test: `an_unreadable_tier_directory_is_reported_not_silently_dropped`.
pub fn roster_from_dirs_reporting(dirs: &[PathBuf]) -> RosterScan {
    let mut by_name: BTreeMap<String, AgentSummary> = BTreeMap::new();
    let mut unreadable: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let scan = scan_agents_reporting(dir);
        unreadable.extend(scan.unreadable);
        for agent in scan.agents {
            by_name
                .entry(agent.name.to_ascii_lowercase())
                .or_insert(agent);
        }
    }
    RosterScan {
        agents: by_name.into_values().collect(),
        unreadable,
    }
}

/// THE agent roster for `project_dir`. Every consumer calls this one function.
///
/// Why (#4588): a roster resolved twice is a roster that drifts. The PM prompt
/// took the three-tier union while `tm session start` printed a count taken
/// from a single directory, so the operator was told 34 when the PM had been
/// given 39 — the two numbers were never wrong in the same way at the same
/// time, because they were never the same computation. There is now exactly one
/// implementation, and `tm session start`, the delegation section injected into
/// the PM prompt, and `tm doctor` all read it.
/// What: unions [`deployed_agent_dirs`] via [`roster_from_dirs`], name-sorted
/// and deduplicated with the highest-precedence tier winning.
/// Test: `session_start_count_matches_the_delivered_delegation_roster`
/// (instruction_pipeline_tests) is the cross-consumer agreement gate; the
/// tier-union behaviour itself is covered by the `roster_from_dirs_*` tests.
/// This function consults machine-global tiers, so it has no hermetic direct
/// test of its own.
pub fn resolve_roster(project_dir: &Path) -> Vec<AgentSummary> {
    resolve_roster_reporting(project_dir).agents
}

/// [`resolve_roster`], additionally reporting what the resolution could not read.
///
/// Why (#5544 review): `resolve_roster` returns a bare `Vec`, so the two
/// consumers that publish its LENGTH — `tm session start`'s
/// `"Instructions: N agents in delegation authority"` (via
/// [`crate::core::instruction_pipeline::PipelineOutput::agent_count`]) and
/// `tm doctor`'s `agents` check — had no way to know the number was short. That
/// made `tm doctor` report a truncated roster as authoritative, which is the
/// remedy the composed prompt's `ROSTER INCOMPLETE` banner points the operator
/// at. Every surface that publishes the count now resolves through here.
/// What: unions [`deployed_agent_dirs`] via [`roster_from_dirs_reporting`],
/// carrying each tier's unreadable paths alongside the merged roster.
/// Test: `session_start_roster_line_declares_an_incomplete_roster`
/// (`commands/session/start.rs`), `agents_check_is_not_ok_when_a_roster_read_failed`
/// (`doctor_fs_checks.rs`), `build_instructions_reports_an_unreadable_agent_file`
/// (`instruction_pipeline_tests.rs`).
pub fn resolve_roster_reporting(project_dir: &Path) -> RosterScan {
    roster_from_dirs_reporting(&deployed_agent_dirs(project_dir))
}

/// Render the LIVE deployed roster for a project, or `None` when none is found.
///
/// Why (#4069): `build_instructions` already computed this section via
/// [`generate_authority`], but its output is merged into a `PipelineOutput`
/// string that the prompt composer never reads — `resolve_pm_prompt` fell back
/// to the static `AGENT_DELEGATION.md` asset, so a 42-agent deployment was
/// advertised to the PM as the asset's hand-maintained 8-row table and agents
/// such as `ticketing` and `memory-manager` were invisible. Regenerating here
/// (rather than threading the pipeline's string down) is the only shape that
/// works for every composer: `build_system_prompt_for*` and its callers —
/// `tm session instructions`, the tmux connect path, the daemon spawn — receive
/// a `project_dir` and nothing else, and two of them never run the pipeline at
/// all, so there is no `PipelineOutput` available to thread.
/// What: resolves the roster via [`resolve_roster`] — the one resolver every
/// consumer shares — and renders it with [`generate_authority`]. Returns `None` when no agent is deployed
/// anywhere, so an unprovisioned environment keeps exactly the previous
/// behaviour (the bundled asset alone) instead of being told it has no agents.
/// Test: `roster_from_dirs_ignores_missing_dirs` (the `None` branch's input) and
/// `instruction_overrides::tests::bundled_delegation_appends_deployed_roster`
/// (the rendered output reaching the delivered prompt). This function itself
/// consults machine-global tiers, so it has no hermetic direct test.
pub fn deployed_roster_section(project_dir: &Path) -> Option<String> {
    let dirs = deployed_agent_dirs(project_dir);
    let section = roster_section_from_dirs(&dirs);
    if section.is_none() {
        // #5544 review (LOW): the tier-only seam cannot name the project, and
        // dropping that field made the fallback event unattributable across
        // concurrent sessions. The project-aware entry point owns this log.
        tracing::info!(
            project = %project_dir.display(),
            tiers = dirs.len(),
            "no deployed agents found in any tier; delegation section falls back to the \
             bundled asset alone"
        );
    }
    section
}

/// [`deployed_roster_section`] with the tiers supplied, so it can be tested.
///
/// Why (#5544): the shipped entry point resolves `$CLAUDE_CONFIG_DIR` and the
/// caller's home directory, which makes every one of its behaviours — including
/// the new incompleteness banner — untestable except by mutating process-global
/// state. Taking the tier list is the seam that removes that coupling; the
/// public function is the same three lines with the tiers resolved.
/// What: scans `dirs` via [`roster_from_dirs_reporting`] and renders through
/// [`generate_authority_reporting`]. Returns `None` ONLY when the scan both
/// found nothing and lost nothing — an all-empty roster that lost paths still
/// renders, because "nothing is deployed" and "nothing could be read" must not
/// collapse into the same silent answer.
/// Test: `an_unreadable_tier_directory_is_reported_not_silently_dropped`,
/// `a_roster_that_is_empty_because_reads_failed_still_renders_a_section`,
/// `roster_section_from_dirs_is_none_when_nothing_is_deployed`.
pub fn roster_section_from_dirs(dirs: &[PathBuf]) -> Option<String> {
    let scan = roster_from_dirs_reporting(dirs);
    let agents = scan.agents;
    if agents.is_empty() && scan.unreadable.is_empty() {
        // Reverting to the bundled asset must be an OBSERVABLE event, not an
        // invisible one — this is the state #4069 describes, and if it recurs
        // the operator needs a thread to pull rather than a silently stale
        // 8-name prompt that looks healthy. `deployed_roster_section` logs it,
        // because only it knows the project. #5544 split the unreadable case
        // out of this branch: an empty roster caused by failed reads now
        // renders a section carrying the banner instead of degrading to `None`.
        return None;
    }
    Some(generate_authority_reporting(&agents, &scan.unreadable))
}

#[cfg(test)]
#[path = "delegation_authority_tests.rs"]
mod tests;
