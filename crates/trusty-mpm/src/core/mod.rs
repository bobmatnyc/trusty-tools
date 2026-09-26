//! # trusty-mpm-core
//!
//! Why: Shared types used by every trusty-mpm crate (daemon, CLI, TUI, Telegram).
//! Centralizing them prevents protocol drift between the daemon and its clients.
//!
//! What: Defines the artifact model (agents, skills, hooks), session state types,
//! and the IPC protocol envelope exchanged over the daemon's local socket / HTTP API.
//!
//! Test: `cargo test -p trusty-mpm-core` exercises serde round-trips and the
//! claude-mpm frontmatter parser against fixture files.

pub mod agent;
pub mod agent_builder;
pub mod agent_cost;
pub mod agent_deployer;
// #7727: every deploy shape resolves `{{TM_SKILLS}}` to files that exist.
pub mod agent_manifest;
pub mod agent_metadata;
pub mod agent_reset;
pub mod agent_reset_workspace;
pub mod agent_skill_codeploy;
#[cfg(test)]
mod skill_root_deploy_tests;
// #4840: bundled agent assets reached `$CLAUDE_CONFIG_DIR/agents/` only via a
// manual `tm install`; this module makes the refresh+deploy automatic.
pub mod agent_source;
// #6495: the classic-renderer default every managed launch carries. Claude
// Code's fullscreen renderer captures the mouse wheel, costing the pane both
// native and tmux scrollback.
pub mod alt_screen;
// #7673: memory files ABOVE the project root — the scan, the launch WARN, and
// the two `tm doctor --fix` repairs (rename a seed aside, exclude a real one).
pub mod ancestor_claude_md;
pub mod ancestor_claude_md_repair;
pub mod artifact;
// #6807: the attribution footer is one constant, shared by the settings seed
// and the `tm pr open` body validator.
pub mod attribution;
pub mod auto_resume;
// #5069: the base checkout behind a worktree owns the embedding lane that
// `worktree_index` deliberately skips; this module is what creates it.
pub mod base_facet_index;
pub mod binary_provenance;
// #7965: the one kill-on-timeout subprocess runner every background sweep uses,
// so a wedged `git`/`gh` child can never pin a blocking-pool thread for the life
// of the daemon.
// #7956: `pub` rather than `pub(crate)` — the `tm` binary is its own
// compilation unit, and `tm wait --for check` needs the same one runner so a
// wedged `gh` cannot carry an invocation past its slice into a SIGKILL.
pub mod bounded_proc;
pub mod budget;
// #6868: the machine-level `build:` section — the shared cargo target
// directory, the job count, and the sccache posture the `rust_build_env` doctor
// row reports and its `--fix` arm seeds.
pub mod build_env;
pub mod build_env_repair;
// #7822: the build fingerprint `tm doctor` compares when two semvers agree —
// a same-version daemon started before the installed binary was written is
// still stale, and semver alone cannot say so.
pub mod build_identity;
pub mod bundle;
// Epic #4183: the DEFAULT (bundled-fallback) PM prompt, re-sourced through
// `instruction_package`. Byte-identical to the legacy assembly it replaces; the
// override configurations stay on that legacy path by design.
pub mod bundled_pm_package;
// #6649: one asset name claimed by two entries inside ONE tier — the collision
// every tier-vs-tier probe is structurally unable to see.
pub mod asset_duplicates;
// #4442/#4448: the ONE bundled-agent name roster, shared by `tm doctor`'s
// asset_tier probe (which reports) and the quarantine (which moves).
pub mod bundled_roster;
// DOC-28 cutover bridge: incremental catch-up runtime — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod catchup;
pub mod circuit;
pub mod claude_config;
// Issue #4467: the shared set of Claude Code process-local session markers every
// managed spawn must scrub. An inherited `CLAUDE_CODE_CHILD_SESSION` makes
// Claude Code disable transcript saving, costing the session native
// `--resume`/`--continue`/`/rewind` recovery.
pub mod claude_env_scrub;
// #7673: the three-outcome downward scan for child git repositories — found,
// clear, or could not finish — behind the workspace-parent seed refusal.
pub mod child_repo_scan;
// Epic #4183 / #4286: the READER for `CLAUDE.md` named-section instruction
// overrides. Ships before the floor text that advertises the mechanism —
// advertising an override no code reads is issue #381 verbatim.
// #7673: Claude Code's `claudeMdExcludes` key — read across layers, extended by
// `tm doctor --fix`.
pub mod claude_md_excludes;
pub mod claude_md_sections;
// #7673: is a CLAUDE.md tm's own seed template, and may tm seed one here? The
// root-cause guard for the $HOME seed-template incident.
pub mod claude_md_seed;
// #7673 round 3: the git-init-offer half of the seed-site guard, split out so
// `claude_md_seed.rs` stays a focused shape/site pair under the SLOC cap.
pub mod claude_md_seed_git;
// Issue #4754: the WRITER counterpart to `claude_md_sections` — the single
// owner of `CLAUDE.md` section-override edits. Idempotent by construction, and
// it borrows the reader's grammar rather than spelling a second one.
pub mod claude_md_writer;
// Issue #4072: one process-wide lock every `~/.claude.json` read-modify-write
// seeder holds, so concurrent daemon provisioning cannot lose a trust entry.
pub mod claude_json_guard;
// DOC-28 cutover bridge: cross-format session discovery — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod claude_mpm_registry;
pub mod claude_mpm_session;

// #6892: the machine-wide builder-slot cap — the `[builders]` config section,
// the memory-tier default table, and the one host-root resolution site.
pub mod builders;

// #8261: the capacity formula that replaces the cap's fixed number — measured
// 1-minute load average and free memory against the operator's ceiling, with
// the fail-closed and never-revoke invariants.
pub mod builder_capacity;

// #8261: the pool of persistent per-slot `CARGO_TARGET_DIR` directories a
// leased builder compiles into, so concurrent builds stop serialising on one
// shared cargo build-directory lock.
pub mod builder_slot_pool;

// #7123: the component labels an issue audit ACCEPTS — the seed table plus the
// repository's own crate labels. Distinct from `policy_labels`, which answers
// which labels the harness CREATES.
pub mod commit_trailers;
pub mod component_labels;
pub mod compress;
pub mod config;
/// Unrecognised-key reporting for the host-level config files (#5207).
pub mod config_keys;
pub mod connect;
pub mod daemon_identity;
pub mod delegation_authority;
pub mod deploy_validate;
pub mod deterministic_overseer;
pub mod discovery;
/// The `disk.max_usage_pct` gate every worktree-creation path consults (#7497).
pub mod disk_usage_guard;
/// Working-tree isolation policy for native Agent-tool dispatches (#4480).
pub mod dispatch_isolation;
pub mod doctor;
pub mod doctor_repair;
// #7678: `tm doctor --fix`'s session-scope arm — re-applies the plugin and MCP
// scope writes to a live project through the launch path's own writers.
pub mod doctor_repair_scope;
pub mod error;
pub mod exit_codes;
pub mod external_session;
pub mod frontmatter;
pub mod gh_account;
// #8510: the gh config dirs an account-only pin may ask for a candidate token.
pub(crate) mod gh_account_dir;
// #8510: proves a candidate token is the pinned account's with `GET /user`.
pub(crate) mod gh_account_proof;
// #5850: the ProjectRegistry half of `gh_account`, read synchronously for a
// daemon-side checkout. `pub(crate)` throughout — nothing outside this crate
// resolves a pin from a bare directory.
pub(crate) mod gh_account_registry;
pub mod gh_identity;
// #7059: the in-process stand-in for the scoped `gh` subprocesses — a test
// seam, compiled out of every `--release` build (see the module docs).
#[cfg(any(test, debug_assertions))]
pub mod gh_scoped_stub;
pub mod git_identity;
// #8511: keep the harness's own files out of every registered project's `git status`.
pub(crate) mod harness_exclude;
pub mod harness_root;
pub mod home_trust_seed;
pub mod hook;
pub mod host_state_gate;
pub mod idle_nudge;
pub mod idle_parking;
// Issue #7616: the compose-time fold — the one transformation between the
// authored section corpus and the bytes delivered to the PM.
pub mod instruction_fold;
pub mod instruction_overrides;
// Issue #4184 / epic #4183: the sectioned-JSON instruction package schema. Types
// + validation only; `bundled_pm_package` is its first composing call site.
pub mod instruction_package;
pub mod instruction_pipeline;
// Issue #8533: the one enumeration of the prompt content no project override
// can remove.
pub mod instruction_safety_core;
// Epic #4183: committed snapshots of the fully composed PM prompt. The
// delivered-prompt diff a content change produces is the review artifact.
pub mod ipc;
// #7097: `tm issue audit` — the mechanical read-back of the #7067 ticketing
// standard. `issue_audit` is the pure evaluation, `issue_audit_gh` the `gh`
// reads that feed it; both live here rather than in `bin/tm` because the
// `issue_audit_recent` doctor check runs from the library.
pub mod issue_audit;
pub mod issue_audit_gh;
pub mod llm_overseer;
// ADR-0055 / #6000: the local-path `repo_url` rule and its typed refusal,
// shared by every `session_new` entry point.
pub mod local_repo_url;
pub mod managed_config;
pub mod manifest;
pub mod mcp_config;
pub mod mcp_provenance;
// #4181: per-project MCP pins now travel as spawn environment variables, not as
// arguments injected into a workspace `.mcp.json` (ADR-0042).
pub mod mcp_session_env;
pub mod mcp_test;
pub mod memory;
pub mod memory_import;
// #7685: `tm memory import-auto-memory` — the migration that makes zeroing
// Claude Code's `MEMORY.md` safe.
pub mod auto_memory_import;
// #7685: auto memory is a FALLBACK — every write site that turns it off asks
// this module whether trusty-memory is available first.
pub mod memory_reachable;
// #8352: `tm memory recall|remember|note` — palace access over the daemon
// socket, so a dead MCP connection does not cut the session off from memory.
pub mod memory_verbs;
pub mod model_inject;
pub mod names;
pub mod oauth_token;
#[cfg(test)]
#[path = "pm_prompt_golden_tests.rs"]
mod pm_prompt_golden_tests;
// DOC-28 cutover bridge: unified session finder — CUTOVER BRIDGE — remove post-migration (#1762)
pub mod native_session_finder;
pub mod output_style;
pub mod output_style_deployer;
pub mod output_style_tiers;
pub mod overseer;
pub mod overseer_config;
// #4058: single canonical source for the crate's own `[[bin]]` names, so
// discovery/hooks/statusline/daemon-PID lists can't drift out of sync again.
pub mod own_binary_names;
pub mod paths;
pub mod pid_registry;
// The labels trusty-mpm applies by policy, shared by session launch and
// `tm issue seed-labels` (#6914). Doc lives in the module's own `//!` header —
// an outer `///` here would resolve its intra-doc links in THIS scope and break
// them (`check_rustdoc_links.sh`).
pub mod policy_labels;
// #7275: deterministic post-merge cleanup — the executor behind
// `tm pr cleanup <n>`, shared by `tm pr merge`'s final step and the
// supervisor's periodic merged-PR sweep.
pub mod pr_cleanup;
pub mod process;
pub mod project;
pub mod project_aliases;
/// The committed, project-level `.trusty-mpm.toml` config surface (#5207).
pub mod project_config;
pub mod project_discovery;
// #7892: `.mcp.json` follows Claude Code's own approval flow, so tm reports
// that state rather than deciding it.
pub mod project_mcp_approval;
// #4880: the project skill tier redeploys on project-manifest change.
pub mod project_skill_tier;
// #6649: `tm doctor --fix-agents` sweeps bundled AGENT copies stranded at the
// project tier — the ledger-proven mirror of `project_tier_strays`.
pub mod project_tier_agent_strays;
// #6586: `tm doctor --fix-skills` sweeps bundled copies stranded at the project
// tier by a pre-#6602 deploy.
pub mod project_tier_strays;
pub mod project_trust;
// #7688: the captured `## Prompt feedback` ledger — extract, append, read back.
pub mod prompt_feedback;
// #7688: the `prompt-self-improvement` flag and the two addenda it injects.
pub mod prompt_self_improvement;
pub mod protected_dirs;
pub mod provisioning_stage;
pub mod push_guard;
// #7171: idempotently disables git's own background maintenance/gc on a
// managed checkout, so an OPERATOR running git directly inside one of its
// worktrees (outside anything that routes through `trusty_common::git`)
// still gets the quieted repo.
pub mod git_maintenance;
// `tm reinstall`: the two-hop asset redeploy across every deploy destination,
// and the install-provenance route its `--binary` flag takes.
pub mod binary_reinstall;
// #4462: whether a `--path` install source is behind `origin/main` — the one
// global `tm` binary regresses every session at once when it is.
pub mod install_freshness;
// #7748: whether the local `origin/<base>` a three-dot diff is taken against is
// the remote's tip — the pre-push credential scan's base, fail-closed.
pub mod base_ref_freshness;
pub mod reinstall;
// #6958: the per-session token-savings ledger every producer appends to, and
// the instruction/language-compression producer that writes the first row.
pub mod savings;
// The `tm compress` tool-output producer, the ledger's third row source — what
// makes the 💸 segment include bash and gate output, not just prompts and reads.
pub mod savings_compress;
// #6959: the bulk-read diversion producer, the ledger's second row source.
pub mod savings_divert;
pub mod savings_instructions;
// #7569: the one-shot repair that quarantines the ledger rows #7514's resolver
// bug let unit tests write into the operator's own ledger.
pub mod savings_repair;
// #7245: the side files that carry an instruction-compression row from the
// compiling process to the hook that learns the session id, and that keep the
// "nothing folded" decline to one warning per project.
pub mod savings_sidecar;
pub mod scaffold_gitignore;
pub mod session;
pub mod session_assets;
pub mod session_launch;
// #7422: default-deny MCP scoping — a session loads the trusty-* builtins, the
// project's own `.mcp.json`, and only the shared servers the project opts into.
pub mod session_mcp_scope;
// #6972: which model the parent session runs, remembered by the statusline hook
// so the divert producer prices its rows at the parent's real rate.
pub mod session_model;
// #8453: the PM or supervisor instruction profile a session runs.
pub mod session_profile;
// #7282: a pause snapshot reaches `origin/main` through its own branch and PR,
// never as a commit on whatever branch the main checkout happens to be on.
pub mod session_pause_pr;
// #7830 / ADR-0062: session history is an orphan, append-only commit chain in
// `refs/tm/sessions/<user>/<key>` — never a commit on a code branch.
pub mod session_ref_publish;
// #7422: the plugin half of the same default-deny decision — written into the
// project's `.claude/settings.json`, because Claude Code has no plugin flag.
pub mod session_plugin_scope;
// #7678: what a project's `.claude/settings.json` still owes that plugin write —
// the one comparison the doctor check, the preview and the writer all read.
pub mod session_scope_drift;
// #7617: which Claude session ids share a managed session, so a restart does not
// read as a savings disappearance.
pub mod session_links;
pub mod session_record;
pub mod session_store;
// #7762: the one cross-process critical section and `.bak`-free atomic publish
// every `settings.json` writer in this crate goes through.
pub(crate) mod settings_lock;
pub mod skill_deploy_tiers;
pub mod skill_deployer;
pub mod skill_drift;
pub mod skill_install_tiers;
pub mod skill_manifest;
// #7751: the stack-to-skill-family table behind the project-tier skillOverrides.
pub mod skill_overrides;
pub mod skill_reconcile;
pub mod skill_repair;
pub mod skill_retire;
pub mod skill_source;
pub mod skill_staleness;
pub mod skill_tiers;
pub mod skill_unmanaged;
pub mod sm;
pub mod spawn_disclaim;
pub mod stack_profile;
// #7424: the turn-1 startup-context reading, its store, and the doctor verdict.
pub mod staged_paths;
pub mod stale_skills;
pub mod standalone;
// #6556: a `SubagentStop` the hook could not deliver, parked on disk where the
// daemon's reap loop replays it — instead of the record sitting Running for the
// six hours of `RUNNING_STALE_AFTER_SECS`.
pub mod startup_context;
pub mod stop_spool;
// #7617: the one seed-or-repair rule for the `statusLine` settings entry, which
// every tier's writer and `tm doctor --fix` go through.
pub mod statusline_settings;
pub mod stray_mcp;
pub mod tmux;
pub mod transcript_usage;
pub mod trusty_tools_config;
// #8572: dirty-tree probe for the main-checkout HEAD-switch guard.
pub mod uncommitted_changes;
pub mod update_check;
pub mod version_staleness;
pub mod workspace_liveness;
pub mod workspace_scan;
// #7889: route (c) of the landing admission — HEAD inside a merged PR's history.
pub mod worktree_carried_by_pr;
pub mod worktree_index;
// #7889: the landed-content admission both reclaim ladders share.
pub mod worktree_landed_content;
// #8633: the merge-into-base question, including a base that moved on.
pub mod worktree_landed_history;
pub mod worktree_naming;
// See ADR-0057 — the facts the pm-guard's removal re-checks ask git and GitHub.
pub mod worktree_removal_facts;

pub use connect::{ResolveResult, SessionSummary, resolve_target};
pub use discovery::{
    DEFAULT_CONSOLE_ADDR, DEFAULT_DAEMON_ADDR, DEFAULT_DAEMON_URL, DaemonUrlError,
    EXIT_DAEMON_URL_UNREACHABLE, GATEWAY_PATH, default_daemon_addr, explicit_url_from_env,
    lock_file_path, resolve_daemon_url, resolve_daemon_url_for_cli, resolve_daemon_url_probing,
    resolve_daemon_url_via_gateway,
};
pub use error::{Error, Result};
