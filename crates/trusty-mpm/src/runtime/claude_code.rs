//! Claude Code runtime adapter.
//!
//! Why: the session manager must have a concrete adapter that launches the
//! `claude` CLI inside a tmux session without leaking `ANTHROPIC_API_KEY` into
//! the pane environment; `env -u ANTHROPIC_API_KEY claude` achieves that.
//! What: [`ClaudeCodeAdapter`] wraps a [`ManagedTmuxDriver`] and implements
//! [`RuntimeAdapter`]; `spawn` sends the env-scrubbed command to the tmux pane
//! after verifying the `claude` binary is on PATH.
//! Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
//! `claude_code_adapter_identifies`.

use std::path::Path;
use std::sync::Arc;

use tracing::debug;

// #7685: imported rather than path-qualified at each use — this file sits at
// its SLOC cap and the qualified form wraps over three lines per call site.
use crate::core::memory_reachable::{resolve_memory_reachable, resolve_spawn_memory_reachable};
use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// #8233: the managed launch is no longer a shell script typed into the pane.
/// `session_id_export_prefix`, `launch_clock_prefix`, `gh_env_source_prefix`,
/// `env_bin_prefix`, `prompt_file_flag`, `cd_and_group`, `exit_dispatch_suffix`,
/// `spawn_command` and `resume_command` all composed pieces of one line whose
/// length grew with the launch and crossed the tty's canonical-mode `MAX_CANON`
/// limit at 1054 bytes. Their behaviour now lives in
/// [`super::managed_launch`] (cwd/env/argv as a [`super::launch_spec::LaunchSpec`])
/// and [`super::launch_report`] (the on-exit hint), both evaluated by the
/// `internal-spawn-disclaimed --launch-spec` shim rather than by the pane shell.
use super::managed_launch::{self, ManagedLaunch};

/// Live-background-session probe and the `attach` relaunch shape (#6863) — a
/// sibling submodule because this file carries a frozen SLOC budget (#2398).
#[path = "claude_code_agents.rs"]
mod claude_code_agents;

// #7568: the prompt-file writer and its named-root seam live in
// `super::prompt_file`; `claude_code.rs` was at the 500-SLOC production cap.
use super::prompt_file::build_prompt_file;

/// Length at which Claude Code truncates a project key and appends a path hash
/// (its `MAX_SANITIZED_LENGTH`). Verified live: a 370-character cwd produced a
/// 207-character directory name — 200 kept characters, `-`, then the hash.
const MAX_PROJECT_KEY_LEN: usize = 200;

/// Encode a workspace path the same way Claude Code names its project dir.
///
/// Why: every on-disk lookup against a Claude Code session store — today only
/// the `--resume <id>`-existence check ([`session_id_exists_in`]) — must derive
/// the SAME project directory name for a given `cwd`. Sharing one helper makes
/// two callers computing the encoding differently impossible.
///
/// What: folds every character outside `[A-Za-z0-9]` to `-`, counting UTF-16
/// code units rather than Rust `char`s, then truncates to
/// [`MAX_PROJECT_KEY_LEN`] plus `-<base36 hash>` when the result is longer.
/// Uppercase is preserved; `/`, `.`, `_`, space, `@`, `+`, `~`, quotes,
/// brackets and every non-ASCII character all become `-`. So
/// `/private/tmp/foo` → `-private-tmp-foo`, and
/// `/repo/.worktrees/w1` → `-repo--worktrees-w1`.
///
/// // #6777: this used to fold `/` alone, so every path segment starting with
/// a dot encoded one character short of the real directory name. Rule
/// re-derived from Claude Code 2.1.260's `sanitizePath`
/// (`s.replace(/[^a-zA-Z0-9]/g, "-")`, then `slice(0,200) + "-" + base36`) and
/// confirmed against live probe runs — see the doc on [`js_path_hash`] for the
/// hash half.
///
/// Test: `encode_project_dir_replaces_slashes`,
/// `encode_project_dir_folds_dot_in_worktrees_path`,
/// `encode_project_dir_folds_every_non_alphanumeric`,
/// `encode_project_dir_folds_astral_char_to_two_dashes`,
/// `encode_project_dir_truncates_and_hashes_a_long_path`,
/// `session_id_exists_finds_hardcoded_dir_name_for_dotted_cwd`,
/// `spawn_resume_uses_resume_flag_for_a_worktree_cwd` (non-circular regression
/// guards against encoding-scheme drift).
///
/// `pub(crate)` (#6777): the daemon's `SessionStart` correlation tests seed
/// transcripts under this same directory name, and a second hand-rolled fold
/// there would drift — it did, and it silently omitted the truncation branch.
pub(crate) fn encode_project_dir(cwd: &Path) -> String {
    let path = cwd.to_string_lossy();
    let mut key = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else {
            // JavaScript's `String.prototype.replace` walks UTF-16 code units,
            // so one astral `char` (a surrogate pair) yields TWO dashes.
            for _ in 0..c.len_utf16() {
                key.push('-');
            }
        }
    }
    if key.len() <= MAX_PROJECT_KEY_LEN {
        return key;
    }
    // `key` is pure ASCII here, so a byte truncation is a character truncation.
    key.truncate(MAX_PROJECT_KEY_LEN);
    key.push('-');
    key.push_str(&to_base36(js_path_hash(&path).unsigned_abs()));
    key
}

/// Claude Code's 32-bit path hash, used only for the over-length key suffix.
///
/// Why: an over-length project key ends in `-<base36 of abs(hash)>`, so
/// reproducing the directory name for a deep worktree requires the exact same
/// hash. tm's own managed worktree paths already reach 188 characters, so this
/// branch is reachable, not theoretical.
/// What: ports `h = (h << 5) - h + unit | 0` over the path's UTF-16 code units.
/// Every JS step truncates to int32, which is wrapping `i32` arithmetic here.
/// Test: `encode_project_dir_truncates_and_hashes_a_long_path`.
fn js_path_hash(path: &str) -> i32 {
    let mut hash: i32 = 0;
    for unit in path.encode_utf16() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    hash
}

/// Render `n` in lowercase base 36, matching JavaScript's `Number#toString(36)`.
///
/// Test: `encode_project_dir_truncates_and_hashes_a_long_path`.
fn to_base36(mut n: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    out.into_iter().map(char::from).collect()
}

// #6765: `has_prior_conversation` / `has_prior_conversation_in` used to live
// here as the `--continue`-eligibility check. They hardcoded
// `~/.claude/projects` — the OPERATOR's store — while every managed relaunch
// exports a managed `CLAUDE_CONFIG_DIR`, so the check answered for one store
// and the flag acted on another. Both relaunch paths now resolve the target
// explicitly (`--resume <id>` verified against the session's OWN store, else a
// fresh launch), leaving neither function a caller. Deleted rather than
// repointed: with no `--continue` branch there is nothing to gate.

/// Resolve the Claude Code `projects` directory used for session storage.
///
/// Why: `CLAUDE_CONFIG_DIR` relocates the ENTIRE config home for a managed
/// session — not just settings/agents/skills but also the conversation-history
/// store under `<config_dir>/projects/` (verified empirically: this daemon's
/// own tool-result artifacts land under
/// `~/.trusty-tools/trusty-mpm/claude-config/projects/<encoded-cwd>/...`). So
/// the existence check for a stored `claude_session_id` (#2013) must look under
/// the SAME `config_dir` the session was/will be launched with, not always
/// `~/.claude/projects` — otherwise every id would appear "missing" for managed
/// sessions and resume would never use `--resume`.
/// What: `<config_dir>/projects` when a managed config dir is resolved; falls
/// back to `~/.claude/projects` when `config_dir` is `None` — the unmanaged
/// default store, which is where an unrelocated `claude` really does keep its
/// conversations.
/// Test: `session_id_exists_prefers_config_dir_projects_when_present`,
/// `session_id_exists_falls_back_to_home_claude_when_no_config_dir`.
fn projects_dir_for(config_dir: Option<&Path>) -> Option<std::path::PathBuf> {
    if let Some(dir) = config_dir {
        return Some(dir.join("projects"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Existence-check a stored `claude_session_id` against Claude Code's local
/// session store before trusting `--resume <id>` (#2013).
///
/// Why: a `claude_session_id` persisted from a prior run can go stale (the
/// session file was pruned, moved, or never made it to disk after a crash).
/// `claude --resume <missing-id>` fails hard with no graceful recovery, which
/// turns `tm` resume into a dead end. Checking first lets the caller fall back
/// to a plain spawn instead of a hard failure.
/// What: best-effort filesystem check — `true` only when
/// `<projects_dir>/<encoded-cwd>/<id>.jsonl` exists as a regular file, where
/// `projects_dir` is resolved via [`projects_dir_for`] and the cwd is encoded
/// via the shared [`encode_project_dir`] helper (every character outside
/// `[A-Za-z0-9]` becomes `-`).
/// #6765: this is now the ONLY conversation-store lookup either relaunch path
/// makes — it is the check that consults the store the spawned process will
/// actually use, which is exactly what the deleted `--continue` gate did not.
/// Never panics: an unresolvable projects dir, a missing file, or any I/O
/// error all conservatively resolve to `false` (safest outcome — it only ever
/// causes an extra fallback, never a hard failure or a wrong `--resume`).
/// Test: `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_false_for_missing_id`,
/// `session_id_exists_false_when_projects_dir_absent`.
///
/// `pub(crate)` (#4337): the daemon's `SessionStart` correlation
/// (`daemon::api::session_start_correlation`) reuses this SAME staleness
/// check to decide whether a managed session's already-stored
/// `claude_session_id` may be replaced by a differently-reported one, so the
/// two call sites can never disagree about what "stale" means.
pub(crate) fn session_id_exists(cwd: &Path, config_dir: Option<&Path>, id: &str) -> bool {
    match projects_dir_for(config_dir) {
        Some(projects_dir) => session_id_exists_in(cwd, &projects_dir, id),
        None => false,
    }
}

/// Inner implementation of the session-id existence check, testable with an
/// injected `projects_dir` (the injected-I/O-root pattern used throughout this
/// module).
///
/// Why: unit tests need to point at a temp directory rather than mutating
/// `HOME`/`CLAUDE_CONFIG_DIR` process-globals.
/// What: encodes `cwd` via [`encode_project_dir`] (every character outside
/// `[A-Za-z0-9]` → `-`) and checks `<projects_dir>/<encoded-cwd>/<id>.jsonl` is
/// a regular file.
/// Test: `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_false_for_missing_id`,
/// `session_id_exists_finds_hardcoded_dir_name_for_known_cwd`,
/// `session_id_exists_finds_hardcoded_dir_name_for_dotted_cwd`.
fn session_id_exists_in(cwd: &Path, projects_dir: &Path, id: &str) -> bool {
    projects_dir
        .join(encode_project_dir(cwd))
        .join(format!("{id}.jsonl"))
        .is_file()
}

/// Resolve, provision, and trust-seed the managed `CLAUDE_CONFIG_DIR` for a spawn.
///
/// Why (DOC-34): every managed spawn points `claude` at the tm-owned config home
/// primarily for AUTH + TRUST isolation — with the API key scrubbed the session
/// authenticates via the keychain/`.credentials.json` keyed to this config-dir
/// path, and its per-workspace trust is seeded into `<config_dir>/.claude.json`
/// (NEVER `~/.claude.json`), so a managed session never reads or writes the
/// operator's `~/.claude`.
///
/// #4873 CORRECTION — the provisioning here is LOAD-BEARING, not
/// belt-and-suspenders. This doc used to say the config home's provisioned
/// agents/skills/settings.json "are NOT loaded" because `--setting-sources
/// project,local` excludes the `user` tier this dir relocates, and that the
/// framework roster arrived via the PROJECT layer instead. That stopped being
/// true when the flag became conditional: all three command builders
/// (`spawn_command`, `resume_command`, `compose_inplace_args`) resolve it
/// through [`crate::core::model_inject::setting_sources_flag`], which returns
/// `--setting-sources user,project,local` whenever `config_dir` is `Some` — and
/// every managed path reaches here with `Some`. The `user` tier IS read. See
/// `spawn_command`'s #4451 note, which already stated this correctly while
/// this comment contradicted it. Auth + trust isolation remains true and is now
/// one of two load-bearing effects, not the only one.
///
/// This centralises the three coupled steps — resolve the path,
/// provision it, and seed workspace trust — so `spawn` and `spawn_resume` stay
/// identical and cannot drift.
/// What: resolves [`crate::core::trusty_tools_config::managed_claude_config_dir`].
/// When `Some`: provisions it via
/// [`crate::core::managed_config::ensure_managed_config_dir`] and seeds managed
/// trust via [`crate::core::standalone::preseed_managed_trust`] (both non-fatal —
/// a failure logs a warning but the dir is still returned so the session never
/// silently falls back to the project's `.claude/`), returning `Some(dir)`. When
/// `None` (home unresolved): falls back to the legacy
/// [`crate::core::home_trust_seed::preseed_home_trust`] and returns `None`
/// (no `CLAUDE_CONFIG_DIR` to inject).
///
/// // #4181 (ADR-0042): this function used to re-run the four `.mcp.json`
/// force-overwrite injectors on every spawn and resume, and derive
/// `enabledMcpjsonServers` from whether each write succeeded (#3918→#3950). Both
/// halves are deleted. MCP servers are declared once in
/// `<config_dir>/.claude.json`'s user-scope `mcpServers` map — the map a
/// relocated spawn reads under `--setting-sources user,project,local`, and which
/// Claude Code connects with no approval — so there is no per-run write to prove
/// and no name to pre-approve. `preseed_managed_trust` now seeds the trust dialog
/// only, and strips any approval an older tm left in the file.
/// Test: exercised via `spawn_sends_env_scrub_when_binary_available` and the
/// `env_set_relocates_the_config_dir` spec-composition test;
/// the provisioning itself is covered in `core::managed_config`;
/// `prepare_managed_config_writes_no_mcp_json` and
/// `prepare_managed_config_writes_no_mcp_approval` cover the deletions.
fn prepare_managed_config(tmux_name: &str, cwd: &Path) -> Option<std::path::PathBuf> {
    prepare_managed_config_with_exe(tmux_name, cwd, None)
}

/// [`prepare_managed_config`] with the hook binary pinned by the caller.
///
/// Why (#7244): provisioning writes the managed hook triad, and that write
/// refuses a build-artifact binary — which a test process is, on a runner with
/// no installed `tm` to fall back to. The provisioning then never reached the
/// `.claude.json` seeding this function's test asserts on.
/// What: the body of [`prepare_managed_config`], forwarding `hook_exe` to
/// [`crate::core::managed_config::ensure_managed_config_dir_with_exe`].
/// Test: `prepare_managed_config_writes_no_mcp_json_and_no_approval`.
fn prepare_managed_config_with_exe(
    tmux_name: &str,
    cwd: &Path,
    hook_exe: Option<&Path>,
) -> Option<std::path::PathBuf> {
    let Some(config_dir) = crate::core::trusty_tools_config::managed_claude_config_dir() else {
        // Home unresolved (stripped env): no config dir to point at. Fall back
        // to the legacy home-trust seed so startup prompts are still dismissed.
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(cwd) {
            tracing::warn!(
                session = %tmux_name,
                cwd = %cwd.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        return None;
    };

    // Provision the tm-owned config dir with the full framework roster. Non-fatal:
    // even on a partial provisioning error we still point CLAUDE_CONFIG_DIR at it,
    // because that is strictly safer than falling back to the project's committed
    // `.claude/` (the #1996 regression this whole change exists to prevent).
    // #4880: `cwd` is the workspace, so the same call also refreshes the
    // PROJECT skill tier (`<cwd>/.claude/skills`) when the project manifest
    // moved — the tier that outranks everything this config dir carries.
    if let Err(e) =
        crate::core::managed_config::ensure_managed_config_dir_with_exe(&config_dir, cwd, hook_exe)
    {
        tracing::warn!(
            session = %tmux_name,
            config_dir = %config_dir.display(),
            "managed config dir provisioning failed (non-fatal): {e}"
        );
    }

    // #7490: re-merge the project-tier hook groups into the project's OWN
    // `.claude/settings.json`. Only `prepare_session` wrote them, and no resume
    // or in-place relaunch reaches it, so a project provisioned before an event
    // group existed never gained it — the `SessionStart` gap that kept the
    // savings row and the 💸 statusline segment from ever appearing. Idempotent
    // (a file that already carries every group is left byte-identical) and
    // non-fatal, like every other step here.
    // The message is terse and the "non-fatal" framing lives in the comment
    // above: this file sits one line under the 500-SLOC cap, and a wrapped
    // `tracing::warn!` costs four more.
    if let Err(e) = crate::core::session_launch::ensure_project_hooks(cwd, hook_exe) {
        tracing::warn!(session = %tmux_name, cwd = %cwd.display(), "hook merge failed: {e}");
    }

    // Seed workspace trust into <config_dir>/.claude.json (isolation invariant:
    // NEVER ~/.claude.json) so the session starts without the trust dialog.
    // // #4181: no MCP approval is written, and a stale one is stripped.
    if let Err(e) = crate::core::standalone::preseed_managed_trust(&config_dir, cwd) {
        tracing::warn!(
            session = %tmux_name,
            cwd = %cwd.display(),
            "managed trust pre-seed failed (non-fatal): {e}"
        );
    }

    Some(config_dir)
}

/// Owned pieces of an in-place `claude` relaunch command, built for direct
/// process `exec` rather than a tmux `send_line` (#2023 component C).
///
/// Why: [`spawn_command`]/[`resume_command`] build single shell STRINGS meant
/// for `tmux send-keys` into a pane whose shell does the quoting/splitting.
/// The bare-`tm` in-pane relaunch instead replaces the CURRENT process image
/// via `std::os::unix::process::CommandExt::exec` — no shell involved, so
/// there is no command string to build (or parse back). This struct carries
/// exactly what the caller (the `tm` CLI binary) needs to construct that
/// [`std::process::Command`] itself.
/// What: `claude_bin` (resolved absolute path), `args` (the isolation flags
/// plus `--resume <id>` or neither, mirroring [`resume_command`]'s
/// selection — see [`compose_inplace_args`]), `config_dir` (the tm-owned
/// `CLAUDE_CONFIG_DIR`, when resolved), and `oauth_token` (issue #2246 — the
/// resolved [`crate::core::oauth_token::resolve_oauth_token`] value, when
/// available). The caller is expected to `env_remove("ANTHROPIC_API_KEY")`
/// and, when each field is `Some`, set the matching env var (`CLAUDE_CONFIG_DIR`
/// / `CLAUDE_CODE_OAUTH_TOKEN`) — the same invariants [`env_bin_prefix`]
/// encodes into the shell-string commands.
/// Test: exercised via [`build_inplace_resume_command`]'s tests.
#[derive(Debug)]
pub struct InPlaceResumeCommand {
    /// Resolved `claude` binary (absolute path).
    pub claude_bin: String,
    /// Full argv (isolation flags + resume-or-fresh selection), EXCLUDING the
    /// binary itself.
    pub args: Vec<String>,
    /// The tm-owned `CLAUDE_CONFIG_DIR`, when resolved (`None` when home is
    /// unresolvable — mirrors [`prepare_managed_config`]'s fallback).
    pub config_dir: Option<std::path::PathBuf>,
    /// The resolved `CLAUDE_CODE_OAUTH_TOKEN`, when available (issue #2246;
    /// see [`crate::core::oauth_token::resolve_oauth_token`]'s precedence).
    pub oauth_token: Option<String>,
    /// The per-project MCP pins to export (#4181): `TRUSTY_MEMORY_PALACE` and
    /// `TRUSTY_INDEX`, as [`crate::core::mcp_session_env::session_mcp_env`]
    /// resolved them. Empty when both manifest toggles are off or neither value
    /// could be derived — the servers then fall back to their own cwd
    /// derivation, which is what the unpinned stub used to do.
    pub mcp_env: Vec<(String, String)>,
}

/// Pure argv composition shared by [`build_inplace_resume_command`] (#2023 C).
///
/// Why: separating the resume/fresh SELECTION from claude-binary
/// resolution (which needs a real `claude` install to exercise end-to-end)
/// keeps the decision itself testable in every CI environment — mirroring how
/// [`resume_command`]'s selection tests pass a fake `claude_bin` string rather
/// than depending on [`ClaudeCodeAdapter::resolve_claude`].
/// What: `--append-system-prompt-file <path>` when `prompt_file` is `Some`
/// (#4336 — see below), then the isolation flags
/// ([`crate::core::model_inject::SETTING_SOURCES_FLAG`] /
/// [`crate::core::model_inject::PERMISSION_MODE_FLAG`], whitespace-split
/// into argv tokens since both constants are simple space-separated flags with
/// no embedded quoting) followed by `--resume <id>` (id exists under
/// `config_dir`, per [`session_id_exists`]) or neither flag (fresh start) — the
/// exact same two-way selection [`resume_command`] makes. #6765: there is no
/// `--continue` fallback on either path; see [`resume_command`]'s doc for why a
/// bare `--continue` under a managed `CLAUDE_CONFIG_DIR` is unsafe.
///
/// `prompt_file` (#4336): this path previously omitted the PM system prompt BY
/// DESIGN, so an in-place relaunch silently restored the operator into vanilla
/// Claude Code — the same defect #2125/#2230 fixed for the spawn and resume
/// paths, left unfixed here because the exec seam is not a shell string. The
/// path is pushed as its OWN argv token and is deliberately NOT run through
/// [`shell_single_quote`] the way [`prompt_file_flag`] does: that helper exists
/// because `spawn_command`/`resume_command` build a string a pane SHELL will
/// re-split, whereas these tokens go straight to `execv` with no shell in
/// between, so quoting them would embed literal quote characters in the
/// filename claude then fails to open.
/// Test: `compose_inplace_args_uses_resume_for_existing_id`,
/// `compose_inplace_args_falls_back_for_missing_id`,
/// `compose_inplace_args_never_continues_from_home_store`,
/// `compose_inplace_args_carries_prompt_file_unquoted`,
/// `compose_inplace_args_omits_prompt_flag_when_absent`.
fn compose_inplace_args(
    cwd: &Path,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
    prompt_file: Option<&Path>,
) -> Vec<String> {
    let effective_id = claude_session_id.filter(|id| session_id_exists(cwd, config_dir, id));

    let mut args: Vec<String> = Vec::new();
    if let Some(prompt) = prompt_file {
        args.push("--append-system-prompt-file".to_owned());
        args.push(prompt.display().to_string());
    }
    args.extend(
        // #4451: in-place relaunch inherits the same relocated-tier contract.
        crate::core::model_inject::setting_sources_flag(config_dir)
            .split_whitespace()
            .chain(crate::core::model_inject::PERMISSION_MODE_FLAG.split_whitespace())
            .map(str::to_owned),
    );
    // #7892: the additive `--mcp-config`, no `--strict-mcp-config`. Unquoted
    // tokens — this path `exec`s with no shell in between, so a quoted path
    // would name a file claude cannot open, exactly as for
    // `--append-system-prompt-file` above.
    args.extend(crate::core::session_mcp_scope::mcp_config_argv(
        crate::core::session_mcp_scope::scoped_for(cwd, config_dir).as_deref(),
    ));

    // #6765: an id verified against the session's OWN store, or a fresh launch.
    // Never a bare `--continue` — see `resume_command`'s doc.
    if let Some(id) = effective_id {
        args.push("--resume".to_owned());
        args.push(id.to_owned());
    }
    args
}

/// Build an [`InPlaceResumeCommand`] for the bare-`tm` in-pane relaunch path
/// (#2023 component C).
///
/// Why: the in-place relaunch must use the SAME `--resume <id>`
/// existence-check → fresh-spawn fallback semantics as the tmux-pane resume
/// path (#2013, #6765) — reusing [`session_id_exists`] /
/// [`prepare_managed_config`] directly (via [`compose_inplace_args`]), rather
/// than re-deriving them, means the two paths can never silently drift.
/// What: resolves the `claude` binary (`Err(RuntimeError::BinaryNotFound)` if
/// missing), provisions/trust-seeds the managed `CLAUDE_CONFIG_DIR` via
/// [`prepare_managed_config`] (logged under the synthetic session name
/// `"in-place-relaunch"` — there is no tmux session name in this context),
/// builds the PM system-prompt file via [`build_prompt_file`] (#4336 — the
/// SAME carrier `spawn`/`spawn_resume` use, previously missing from this path
/// alone; non-fatal, a write failure omits the flag), then delegates argv
/// composition to [`compose_inplace_args`].
/// Test: `build_inplace_resume_command_resolves_claude_binary`,
/// `build_inplace_resume_command_carries_prompt_file`.
pub fn build_inplace_resume_command(
    cwd: &Path,
    claude_session_id: Option<&str>,
) -> Result<InPlaceResumeCommand, RuntimeError> {
    let config_dir = prepare_managed_config("in-place-relaunch", cwd);
    // #7422: compose the session-scoped MCP file BEFORE anything else, so the
    // fail-closed gate does not depend on a binary lookup succeeding first. A
    // failure here abandons the relaunch rather than dropping the flag, which
    // would hand the pane the unscoped shared server map.
    crate::core::session_mcp_scope::provision_for_spawn(cwd, config_dir.as_deref())
        .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
    let claude_bin = ClaudeCodeAdapter::resolve_claude().ok_or_else(|| {
        RuntimeError::BinaryNotFound(
            "claude binary not found on PATH or in well-known dirs \
             (e.g. ~/.local/bin) — install Claude Code first"
                .into(),
        )
    })?;
    // #4832: no explicit id here — this path runs INSIDE the managed pane, so
    // `session_scope` reads `TM_MANAGED_SESSION_ID` from the environment.
    let prompt_file = build_prompt_file(cwd, None);
    let args = compose_inplace_args(
        cwd,
        config_dir.as_deref(),
        claude_session_id,
        prompt_file.as_deref(),
    );
    let oauth_token = crate::core::oauth_token::resolve_oauth_token();
    // #4181: same per-project MCP pins the tmux-pane paths export.
    let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);
    Ok(InPlaceResumeCommand {
        claude_bin,
        args,
        config_dir,
        oauth_token,
        mcp_env,
    })
}

/// Runtime adapter that launches Claude Code CLI inside a tmux session.
///
/// Why: Claude Code is the primary agent runtime for MPM sessions; coupling the
/// launch sequence (binary check, env scrub, tmux send) to a typed adapter keeps
/// the session manager free of runtime-specific knowledge.
/// What: holds a [`ManagedTmuxDriver`] reference; `spawn` verifies the `claude`
/// binary exists, then sends `env -u ANTHROPIC_API_KEY claude` to the named pane.
/// Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
/// `claude_code_adapter_identifies`.
pub struct ClaudeCodeAdapter {
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
    /// What the launch already resolved about trusty-memory (#7685); `None`
    /// means this adapter must ask the host itself.
    memory_reachable: Option<bool>,
}

impl ClaudeCodeAdapter {
    /// Construct an adapter backed by the given tmux driver.
    ///
    /// Why: the session manager injects the tmux driver via `Arc<dyn …>` so
    /// the adapter is testable without a real tmux binary. `memory_reachable`
    /// (#7685) is what this launch's `prepare_session_inner` already answered one
    /// `PROBE_TIMEOUT` ago; taking it at CONSTRUCTION rather than offering a
    /// setter is what stops a caller spawning before pinning it. `None` — from a
    /// caller that ran no preparation — keeps the probe, so the answer is never
    /// guessed.
    /// What: stores both.
    /// Test: used in every `ClaudeCodeAdapter` test;
    /// `spawn_uses_the_launch_resolved_reachability` pins the reachability half.
    pub fn new(
        tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
        memory_reachable: Option<bool>,
    ) -> Self {
        Self {
            tmux,
            memory_reachable,
        }
    }

    /// Resolve the `claude` binary to an absolute path, or `None` if missing.
    ///
    /// Why: under launchd the daemon (and the tmux pane it spawns) inherits a
    /// minimal `PATH` that omits `~/.local/bin` where Claude Code installs, so a
    /// bare `claude` on the pane would fail to launch (spawn `[errored]`, #1298).
    /// Resolving to an absolute path here lets the spawn command invoke claude
    /// by full path, independent of the pane's `PATH`.
    /// What: delegates to [`trusty_common::bin_resolve::resolve_binary`] which
    /// checks the live `PATH` first then the well-known daemon dirs (Homebrew +
    /// `~/.local/bin` + `~/.cargo/bin`); returns the resolved path as a `String`.
    /// Test: `claude_code_adapter_binary_check_returns_option`.
    fn resolve_claude() -> Option<String> {
        trusty_common::bin_resolve::resolve_binary("claude")
            .and_then(|p| p.to_str().map(str::to_owned))
    }

    /// Durably publish `TM_MANAGED_SESSION_ID` (and `CLAUDE_CONFIG_DIR` when
    /// resolved) into the tmux SESSION environment (#2157 item 1).
    ///
    /// Why: [`session_id_export_prefix`] only lands in the ONE pane shell that
    /// runs the spawn/resume command line — a sibling pane/window in the same
    /// tmux session, or a pane spawned by a pre-#2157 build, never sees it. This
    /// is belt-and-suspenders alongside that export: `tmux set-environment`
    /// writes into the session's own environment table, which
    /// `tmux show-environment` can read from ANY pane in the session — the
    /// fallback the in-place-relaunch gate (`bin/tm/commands/guided_inplace.rs`)
    /// now uses when the process environment is empty.
    /// What: best-effort — a failure is logged at `warn` and never propagated;
    /// the pane-shell export remains the primary mechanism, so a tmux driver
    /// that cannot support `set_environment` must not fail the spawn/resume it
    /// is attached to.
    /// Test: `spawn_publishes_session_id_via_set_environment`,
    /// `spawn_resume_publishes_session_id_via_set_environment`.
    fn publish_session_env(&self, tmux_name: &str, session_id: &str, config_dir: Option<&str>) {
        if let Err(e) = self
            .tmux
            .set_environment(tmux_name, "TM_MANAGED_SESSION_ID", session_id)
        {
            tracing::warn!(
                session = %tmux_name,
                "tmux set-environment TM_MANAGED_SESSION_ID failed (in-place relaunch \
                 fallback impaired, non-fatal): {e}"
            );
        }
        if let Some(dir) = config_dir
            && let Err(e) = self
                .tmux
                .set_environment(tmux_name, "CLAUDE_CONFIG_DIR", dir)
        {
            tracing::warn!(
                session = %tmux_name,
                "tmux set-environment CLAUDE_CONFIG_DIR failed (non-fatal): {e}"
            );
        }
    }
}

impl RuntimeAdapter for ClaudeCodeAdapter {
    /// Launch Claude Code in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// actual agent process starts inside it.
    /// What: resolves `claude` to an absolute path (returns `BinaryNotFound`
    /// if it cannot be found on `PATH` or in the well-known daemon dirs),
    /// provisions + trust-seeds the tm-owned `CLAUDE_CONFIG_DIR` via
    /// [`prepare_managed_config`], builds the PM system-prompt file via
    /// [`build_prompt_file`] (issue #2125 item 3), resolves an optional
    /// `CLAUDE_CODE_OAUTH_TOKEN` via
    /// [`crate::core::oauth_token::resolve_oauth_token`] (issue #2246), then
    /// sends a FIXED-SHAPE launch line naming a [`super::launch_spec::LaunchSpec`] the shim reads
    /// ([`managed_launch::deliver`]); the task is
    /// logged for observability but not passed to the command. `gh_env`
    /// (#3025) is caller-RESOLVED (the daemon's spawn handler consults the
    /// `ProjectRegistry` — the actual write target for a pinned `gh_account`
    /// — off the async executor via `tokio::task::spawn_blocking`, since this
    /// trait method itself is synchronous); #8233: it rides in the spec's
    /// `env_set` (a mode-0600 file inside a mode-0700 directory, consumed and
    /// deleted by the shim) rather than in any typed text, so the token still
    /// never reaches the pane's shell history or any process's argv.
    /// Test: `spawn_sends_the_parameterized_launch_line`,
    /// `spawn_errors_when_the_line_is_refused`.
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
        gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        // Point the session at the tm-owned CLAUDE_CONFIG_DIR for auth + trust
        // isolation and seed trust there — never at `~/.claude.json` (DOC-34).
        // #4873: the framework roster and skills load FROM this config dir —
        // `setting_sources_flag` yields `user,project,local` whenever it is
        // present, and `user` is the tier it relocates. The older comment here
        // claimed the project layer under `project,local`; see
        // `prepare_managed_config`. Non-fatal throughout (closes #1696).
        let config_dir = prepare_managed_config(tmux_name, cwd);
        // #7422: compose this session's MCP config before anything else, so the
        // fail-closed gate does not depend on a binary lookup succeeding first.
        // Fatal by design — the only fallback is a spawn with no
        // `--mcp-config`, which loads the whole shared server map.
        crate::core::session_mcp_scope::provision_for_spawn(cwd, config_dir.as_deref())
            .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            "spawning claude-code in tmux pane"
        );
        // Build and inject the PM system prompt (issue #2125 item 3) so this,
        // the default daemon on-ramp, can no longer silently spawn vanilla
        // Claude Code. Non-fatal: a write failure omits the flag (#2173 ruled
        // out a CLAUDE.md-carrier fallback, so there is no other carrier).
        let prompt_file = build_prompt_file(cwd, Some(session_id));
        // Issue #2246: inject CLAUDE_CODE_OAUTH_TOKEN when one is available
        // (an operator-set env var, else the tm-managed store) to bypass the
        // CLAUDE_CONFIG_DIR-keyed Keychain divergence that causes the
        // managed-session login loop. `None` when neither source has a
        // token — the command is then byte-identical to pre-#2246.
        let oauth_token = crate::core::oauth_token::resolve_oauth_token();
        // Issue #3025: `gh_env` was already resolved by the caller (registry
        // lookup + `gh auth token` both happen off the async executor).
        // #8233: it now rides in the launch spec's `env_set`/`env_unset`, which
        // is the same mode-0600 carrier the OAuth token uses — one secret file
        // per launch instead of two, still never typed into the pane.
        // #4181: the per-project MCP pins the shared user-scope declarations
        // cannot carry as arguments. Resolved once per spawn (it touches the
        // trusty-search daemon), never inside the spec builder.
        let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);
        // #7685: auto memory is the FALLBACK, so the kill switch goes into the
        // spec only when trusty-memory answered. The launch resolved this
        // already where it could; this only probes when nothing did, so the
        // spec builders below stay pure functions of their arguments.
        let memory_reachable = resolve_spawn_memory_reachable(self.memory_reachable);
        // #2997: `claude_bin` is passed UNWRAPPED — the shim is now the process
        // that reads the spec, so it `posix_spawn`s `claude` itself, disclaimed,
        // with one hop less than the old `env`-as-program shape.
        let launch = ManagedLaunch {
            cwd,
            claude_bin: &claude_bin,
            config_dir: config_dir.as_deref(),
            session_id,
            prompt_file: prompt_file.as_deref(),
            oauth_token: oauth_token.as_deref(),
            gh_env,
            mcp_env: &mcp_env,
            memory_reachable,
        };
        managed_launch::deliver(
            self.tmux.as_ref(),
            tmux_name,
            None,
            &managed_launch::spawn_spec(&launch),
        )?;
        // #2157 item 1: durable publish, belt-and-suspenders alongside the
        // pane-shell export the launch line still carries.
        self.publish_session_env(
            tmux_name,
            session_id,
            config_dir.as_deref().and_then(|p| p.to_str()),
        );
        Ok(())
    }

    /// Resume Claude Code with conversation continuity (#1744, #2013, #6765).
    ///
    /// Why: `resume_managed` must restore the prior conversation rather than
    /// starting fresh. If the stored `claude_session_id` is available AND
    /// still resolves to a real session on disk (checked via
    /// [`session_id_exists`], #2013), `--resume <id>` restores the exact
    /// conversation. A stale id (the session was pruned, moved, or never
    /// reached disk) is NOT passed to `--resume` — `claude --resume <missing>`
    /// fails hard with no recovery — instead the pane launches FRESH. #6765:
    /// there is no `--continue` fallback; the target is resolved explicitly or
    /// not at all, so the decision never depends on which conversation the
    /// managed store happens to consider "most recent". #6863: the disk check
    /// alone cannot tell a FINISHED conversation from one Claude Code is still
    /// running as a background job — the `.jsonl` exists either way, and the
    /// live one refuses `--resume` — so the choice now runs through
    /// [`claude_code_agents::relaunch_command`], which attaches instead.
    /// What: resolves the claude binary, provisions + trust-seeds the tm-owned
    /// `CLAUDE_CONFIG_DIR` via [`prepare_managed_config`], existence-checks
    /// `claude_session_id` against the resolved config dir, falls back to a
    /// fresh launch when it is missing,
    /// builds the PM system-prompt file via [`build_prompt_file`] (#2230 —
    /// same carrier `spawn` uses, previously missing from every resume path),
    /// resolves an optional `CLAUDE_CODE_OAUTH_TOKEN` via
    /// [`crate::core::oauth_token::resolve_oauth_token`] (#2246 — same carrier
    /// `spawn` uses), then sends the appropriate [`resume_command`] to the
    /// tmux pane — targeting the SPECIFIC `pane_id` (via
    /// [`ManagedTmuxDriver::send_line_to_pane`]) when the caller supplies one,
    /// rather than the session-scoped [`ManagedTmuxDriver::send_line`], which
    /// tmux resolves to whichever pane/window is currently active (sibling-
    /// window hijack fix, follow-up to #2456). `pane_id: None` (a legacy
    /// record that predates pane-id capture) falls back to the session-scoped
    /// send, preserving prior behavior for that case.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_no_prior_conv_sends_plain_spawn`,
    /// `spawn_resume_never_sends_bare_continue`,
    /// `spawn_resume_sends_prompt_file_when_binary_available`,
    /// `spawn_resume_sends_oauth_token_when_available`,
    /// `spawn_resume_targets_stored_pane_id_when_known`,
    /// `spawn_resume_falls_back_to_session_target_when_pane_id_unknown`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_resume(
        &self,
        tmux_name: &str,
        pane_id: Option<&str>,
        cwd: &Path,
        task: &str,
        claude_session_id: Option<&str>,
        session_id: &str,
        gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        let config_dir = prepare_managed_config(tmux_name, cwd);
        // #7422: same fail-closed MCP composition as `spawn`, in the same
        // position — a resumed pane must not be the one path that still loads
        // every shared server.
        crate::core::session_mcp_scope::provision_for_spawn(cwd, config_dir.as_deref())
            .map_err(|err| RuntimeError::Spawn(err.to_string()))?;
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        // #2230: build the PM system prompt for the resume path too — before
        // this fix only spawn() passed --append-system-prompt-file, so every
        // resumed/guided-resume/crash-recovery session silently ran vanilla
        // Claude Code. Non-fatal: a write failure omits the flag.
        let prompt_file = build_prompt_file(cwd, Some(session_id));
        // #2246: the resume path must ALSO carry CLAUDE_CODE_OAUTH_TOKEN —
        // every resumed/guided-resume/crash-recovery session funnels through
        // here, so omitting it would leave exactly those sessions exposed to
        // the login loop spawn() itself was fixed against.
        let oauth_token = crate::core::oauth_token::resolve_oauth_token();
        // Issue #3025: same caller-resolved `gh_env` as `spawn` — every resumed /
        // guided-resume / crash-recovery session must get the same deterministic
        // `gh` identity, and #8233 carries it in the launch spec, not a second
        // temp file the pane sources.
        // #4181: the per-project MCP pins the shared user-scope declarations
        // cannot carry as arguments. Resolved once per spawn (it touches the
        // trusty-search daemon), never inside the spec builder.
        let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);

        // #2013: a stored id can go stale — existence-check it before trusting
        // `--resume <id>` so a missing session falls back gracefully instead
        // of a hard `claude` failure.
        let effective_id = claude_session_id.filter(|id| {
            let exists = session_id_exists(cwd, config_dir.as_deref(), id);
            if !exists {
                tracing::warn!(
                    session = %tmux_name,
                    claude_session_id = %id,
                    "stored claude_session_id no longer resolves to a session on \
                     disk; falling back instead of passing --resume (#2013)"
                );
            }
            exists
        });
        if let Some(id) = effective_id {
            tracing::warn!(
                session = %tmux_name,
                claude_session_id = %id,
                "resuming with --resume <id>; if conversation no longer exists on \
                 disk, Claude Code will error — the reap loop will detect and mark \
                 Stopped within ~60 s (#1744)"
            );
        }
        // #6765: no `--continue` fallback — a usable id resumes by id, anything
        // else launches fresh.
        debug!(
            session = %tmux_name,
            pane_id = pane_id.unwrap_or("<none>"),
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            resume = effective_id.is_some(),
            "resuming claude-code in tmux pane"
        );
        // #2997: `claude_bin` stays UNWRAPPED — the shim reads the spec and
        // `posix_spawn`s `claude` disclaimed itself, so every resumed /
        // guided-resume / crash-recovery pane keeps the same TCC contract.
        let launch = ManagedLaunch {
            cwd,
            claude_bin: &claude_bin,
            config_dir: config_dir.as_deref(),
            session_id,
            prompt_file: prompt_file.as_deref(),
            oauth_token: oauth_token.as_deref(),
            gh_env,
            mcp_env: &mcp_env,
            // #7685: same fallback rule as `spawn` — a resumed session must not
            // lose auto memory while trusty-memory is down either.
            memory_reachable: resolve_memory_reachable(self.memory_reachable),
        };
        // #6863: a session Claude Code is still running in the background
        // refuses `--resume` and exits 0, leaving the pane a bare shell; ask its
        // own registry first and `attach` instead. Any failure to read the
        // registry falls back to the pre-#6863 `--resume`/fresh-launch choice.
        // The probe is passed unevaluated: a launch with no stored id has
        // nothing to look up and must not spawn `claude` at all.
        let spec =
            claude_code_agents::relaunch_spec(&launch, claude_session_id, effective_id, || {
                claude_code_agents::query_registry(&claude_bin, config_dir.as_deref())
            });
        // Sibling-window hijack fix (follow-up to #2456): when the caller
        // supplies the record's own `pane_id`, target it directly — tmux's
        // session-scoped send resolves to whichever pane/window is currently
        // ACTIVE, which is not necessarily (and after this bug, often was not)
        // the pane this resume is actually about. `None` (a legacy record
        // predating pane-id capture) preserves the prior session-scoped
        // behavior — there is no stronger signal available.
        managed_launch::deliver(self.tmux.as_ref(), tmux_name, pane_id, &spec)?;
        // #2157 item 1: durable publish for the RESUME path too — a fresh tmux
        // session is created on resume, so it needs the same belt-and-suspenders
        // set-environment call as spawn().
        self.publish_session_env(
            tmux_name,
            session_id,
            config_dir.as_deref().and_then(|p| p.to_str()),
        );
        Ok(())
    }

    /// Return `"claude-code"` as the adapter's identifier.
    ///
    /// Why: logs and status responses must identify this adapter by name so
    /// operators can distinguish it from future runtimes.
    /// What: static string, no I/O.
    /// Test: `claude_code_adapter_identifies`.
    fn identify(&self) -> &str {
        "claude-code"
    }
}

// The command-builder and `RuntimeAdapter` impl tests (#3070) live in a
// sibling `_tests.rs` file for the same reason — see that file's module doc.
#[cfg(test)]
#[path = "claude_code_tests.rs"]
mod tests;

// #7422: the default-deny MCP flags get their own file — one pin per builder,
// so an edit that drops them from one path cannot hide behind the others.
#[cfg(test)]
#[path = "claude_code_scope_tests.rs"]
mod scope_tests;
