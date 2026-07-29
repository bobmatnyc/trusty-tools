//! Built-in skills for orchestration, session control, ticketing and MCP admin.
//!
//! Why: These tools are how an agent reaches *outside its own turn* — it
//! delegates, drives tmux sessions, moves tickets, or reconfigures the MCP
//! surface. Most are `System` kind so the pane can collapse them away from the
//! capabilities a user actually asks for.
//! What: A `const` table of one-tool [`SkillDef`] rows.
//! Test: `super::super::tests::every_tool_declared_in_source_has_a_skill`.

use super::super::{SkillDef, SkillKind::Action, SkillKind::System, tool_skill};

pub(super) static TABLE: &[SkillDef] = &[
    // --- delegation / workflow ------------------------------------------
    tool_skill(
        "delegate-specialist",
        "Delegate to a Specialist",
        "Hand a task to another agent (engineer, QA, research) and return its answer.",
        "delegate_to_agent",
        Action,
        None,
    ),
    tool_skill(
        "task-dispatch",
        "Dispatch a Task",
        "Send a task to the project-manager bridge for scheduling.",
        "dispatch_task",
        System,
        None,
    ),
    tool_skill(
        "workflow-advance",
        "Advance the Workflow Phase",
        "Move a workflow-engine run to its next phase after an audit.",
        "advance_workflow_phase",
        System,
        None,
    ),
    tool_skill(
        "task-finish",
        "Finish the Task",
        "Declare the current task complete and return the final answer.",
        "finish_task",
        System,
        None,
    ),
    tool_skill(
        "system-health",
        "System Status",
        "Report the health of the local trusty-* daemons, MCP servers and credentials.",
        "system_status",
        System,
        None,
    ),
    // --- skill discovery -------------------------------------------------
    tool_skill(
        "skill-list",
        "List Available Skills",
        "List the skills discoverable from this agent's skill sources.",
        "list_skills",
        System,
        None,
    ),
    tool_skill(
        "skill-load",
        "Load a Skill",
        "Load one skill's guidance into the current conversation.",
        "load_skill",
        System,
        None,
    ),
    // --- CTRL project management ----------------------------------------
    tool_skill(
        "project-list",
        "List Projects",
        "List the projects registered with the local harness.",
        "list_projects",
        System,
        None,
    ),
    tool_skill(
        "project-add",
        "Register a Project",
        "Register a new project directory with the local harness.",
        "add_project",
        System,
        None,
    ),
    tool_skill(
        "project-remove",
        "Unregister a Project",
        "Remove a project from the local harness registry.",
        "remove_project",
        System,
        None,
    ),
    tool_skill(
        "project-activate",
        "Set the Active Project",
        "Make one registered project the active working context.",
        "set_active_project",
        System,
        None,
    ),
    tool_skill(
        "project-self-status",
        "Self-Project Status",
        "Report the harness's own repository state and open work.",
        "self_project_status",
        System,
        None,
    ),
    tool_skill(
        "project-self-task",
        "Start a Self-Improvement Task",
        "Start a task against the harness's own repository.",
        "initiate_self_task",
        System,
        None,
    ),
    // --- CTRL session management ----------------------------------------
    tool_skill(
        "session-start-pm",
        "Start a PM Session",
        "Launch a project-manager session for a project.",
        "start_pm",
        System,
        None,
    ),
    tool_skill(
        "session-task-status",
        "Task Status",
        "Report the state of a running task.",
        "task_status",
        System,
        None,
    ),
    tool_skill(
        "session-stop-task",
        "Stop a Task",
        "Stop a running task.",
        "stop_task",
        System,
        None,
    ),
    // --- L0 read-only session state (#4171, epic #4167) ------------------
    // One skill per tool (owner ruling). These three wrap the L0-ONLY
    // read-only session-state executors; the tier gate lives in
    // `tools::session_state`, not here — a skill grant can never widen past
    // it, because `retain_tier_permitted` runs on the compiled-down tool
    // names AFTER `[skills].allow` has been expanded.
    tool_skill(
        "session-state-list",
        "List Orchestration Sessions",
        "List the orchestration sessions recorded on this machine, most recently active first.",
        "session_state_list",
        System,
        None,
    ),
    tool_skill(
        "session-state-status",
        "Orchestration Session Status",
        "Report one orchestration session's recorded state, branch, workspace and pending decision.",
        "session_state_status",
        System,
        None,
    ),
    tool_skill(
        "session-state-snapshot",
        "Read Session Snapshots",
        "List or read this project's own recorded session artefacts (scrollback, instructions, write-ups).",
        "session_state_snapshot",
        System,
        None,
    ),
    // --- tmux session control -------------------------------------------
    tool_skill(
        "tmux-session-list",
        "List tmux Sessions",
        "List the harness's tmux sessions.",
        "tm_list_sessions",
        System,
        None,
    ),
    tool_skill(
        "tmux-project-list",
        "List tmux Projects",
        "List the projects with tmux sessions.",
        "tm_list_projects",
        System,
        None,
    ),
    tool_skill(
        "tmux-pane-capture",
        "Capture a tmux Pane",
        "Read the visible contents of a tmux pane.",
        "tm_capture_pane",
        System,
        None,
    ),
    tool_skill(
        "tmux-reconcile",
        "Reconcile tmux State",
        "Re-sync recorded session state against live tmux.",
        "tm_reconcile",
        System,
        None,
    ),
    tool_skill(
        "tmux-session-new",
        "Create a tmux Session",
        "Start a new tmux session for a project.",
        "tm_new_session",
        System,
        None,
    ),
    tool_skill(
        "tmux-session-kill",
        "Kill a tmux Session",
        "Terminate a tmux session.",
        "tm_kill_session",
        System,
        None,
    ),
    tool_skill(
        "tmux-session-pause",
        "Pause a tmux Session",
        "Pause a running tmux session.",
        "tm_pause_session",
        System,
        None,
    ),
    tool_skill(
        "tmux-session-resume",
        "Resume a tmux Session",
        "Resume a paused tmux session.",
        "tm_resume_session",
        System,
        None,
    ),
    tool_skill(
        "tmux-session-send",
        "Message a tmux Session",
        "Send a message into a running tmux session.",
        "tm_send_message",
        System,
        None,
    ),
    // --- ticketing (REMOVED — ADR-0024 decision 4, owner 2026-07-29) -------
    //
    // The twelve leaf rows that lived here (`ticket-create`, `ticket-read`,
    // `ticket-update`, `ticket-close`, `ticket-list`, `ticket-comment`,
    // `ticket-tag`, `ticket-assign`, `ticket-transition`, `ticket-search`,
    // `ci-run-trigger`, `ci-run-status`) were deleted with the `ticketing`
    // function-skill row that bundled them (`super::functions`). The owner's
    // ruling: ticketing is reachable as a SUB-AGENT only, never as a skill, so
    // it must not surface as a skill card — and `skills_at`
    // (`api::server::agent_skills`) renders EVERY manifest row, granted or not,
    // so removing the rows is the only way to remove the cards.
    //
    // The TOOLS are untouched and still registered
    // (`crate::tools::native_ticketing`); `ticketing-agent.toml` grants them
    // by name in its own `[tools].allowed`, which does not go through this
    // catalog. This comment is left in place of the rows so the next reader
    // finds the decision rather than an unexplained gap between `tmux-*` and
    // `mcp-*`.
    // --- MCP administration ----------------------------------------------
    tool_skill(
        "mcp-list",
        "List MCP Services",
        "List the configured MCP services and their state.",
        "mcp_list",
        System,
        None,
    ),
    tool_skill(
        "mcp-add",
        "Add an MCP Service",
        "Register a new MCP service.",
        "mcp_add",
        System,
        None,
    ),
    tool_skill(
        "mcp-remove",
        "Remove an MCP Service",
        "Unregister an MCP service.",
        "mcp_remove",
        System,
        None,
    ),
    tool_skill(
        "mcp-enable",
        "Enable an MCP Service",
        "Enable a configured MCP service.",
        "mcp_enable",
        System,
        None,
    ),
    tool_skill(
        "mcp-disable",
        "Disable an MCP Service",
        "Disable a configured MCP service.",
        "mcp_disable",
        System,
        None,
    ),
];
