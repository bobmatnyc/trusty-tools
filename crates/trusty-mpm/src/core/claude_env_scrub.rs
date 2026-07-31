//! Scrub Claude Code's process-local session markers from a managed spawn's
//! environment (issue #4467).
//!
//! Why: when `tm` is invoked from inside a Claude Code session — the PM
//! delegating, the daemon having been started from a session, or an operator
//! simply running `tm` in a `claude` pane — it inherits the markers Claude Code
//! injects into every child process it spawns. `tm` then passed that whole
//! environment through to the `claude` it spawns, and the inherited
//! [`TRANSCRIPT_SUPPRESSING_MARKER`] made Claude Code turn session persistence
//! OFF for the managed session:
//!
//! > Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker
//!
//! The consequence was silent: managed sessions got no native `--resume` /
//! `--continue` / `/rewind` recovery and never appeared in `--resume`, so a
//! session that died lost everything since tm's last (summary-only) snapshot.
//!
//! The marker set below is not guesswork. It is exactly what Claude Code's own
//! child-process env injector sets, read out of the `claude` 2.1.220 bundle:
//!
//! ```text
//! function KDt(e){ let t={CLAUDECODE:"1", CLAUDE_CODE_SESSION_ID:e.sessionId,
//!   CLAUDE_CODE_CHILD_SESSION:"1", CLAUDE_PID:String(process.pid)};
//!   if(e.effortLevel!==void 0) t.CLAUDE_EFFORT=e.effortLevel; ... }
//! ```
//!
//! plus `CLAUDE_CODE_EXECPATH` (the parent interpreter's path), which appears on
//! that same bundle's own list of variables it refuses to propagate into a
//! shell snapshot. Every entry names the PARENT process — its session id, its
//! pid, its binary, its current turn's effort level — so none of them can be
//! correct for a fresh managed session.
//!
//! What this module deliberately does NOT scrub is as load-bearing as what it
//! does: see [`DELIBERATE_SPAWN_ENV`]. A blanket scrub of `CLAUDE_*` would take
//! `CLAUDE_CONFIG_DIR` with it and instantly re-break issue #4451 — #4455
//! depends on that relocation to put the bundled agent roster in a settings
//! tier a managed session actually loads.
//!
//! Test: `crates/trusty-mpm/src/core/claude_env_scrub_tests.rs`.

/// The marker whose INHERITED presence disables Claude Code transcript saving.
///
/// Why: this is the single trigger, not one of several. The gate in the `claude`
/// 2.1.220 bundle reads:
///
/// ```text
/// function Zkt(){
///   if (Z.CLAUDE_CODE_FORCE_SESSION_PERSISTENCE) return false;
///   if (!(Z.CLAUDE_CODE_CHILD_SESSION && PN() && !o_())) return false;
///   return !Tsg();   // Tsg(): tmux show-environment -g CLAUDE_CODE_CHILD_SESSION
/// }
/// ```
///
/// Named separately from the rest of [`INHERITED_SESSION_MARKERS`] because the
/// `tm doctor` probe asserts specifically on THIS variable: the others are
/// correctness hygiene, this one is the difference between a recoverable
/// managed session and an unrecoverable one.
///
/// Note the upstream escape hatch `Tsg()`: a marker present in the tmux SERVER's
/// global environment suppresses the heuristic. That is why the defect looked
/// intermittent rather than universal — a tmux server started from inside a
/// Claude Code session carries the marker globally and masks the leak. Relying
/// on that mask is not an option: it depends on how the tmux server happened to
/// be started, so scrubbing the variable is what makes the outcome
/// deterministic.
/// What: the literal variable name.
/// Test: `suppressing_marker_is_scrubbed`.
pub const TRANSCRIPT_SUPPRESSING_MARKER: &str = "CLAUDE_CODE_CHILD_SESSION";

/// Claude Code's process-local markers, which a managed spawn must not inherit.
///
/// Why: see the module doc — every entry is set by Claude Code's own
/// child-process env injector and describes the PARENT process, so inheriting
/// any of them mis-attributes the new session. `CLAUDE_CODE_CHILD_SESSION`
/// additionally disables transcript saving outright (issue #4467).
/// What: the marker names, in the order they appear on the spawn's `env -u`
/// prefix. [`TRANSCRIPT_SUPPRESSING_MARKER`] leads so the most consequential
/// entry is the first one a reader (or a pane's shell history) sees.
/// Test: `marker_list_is_free_of_deliberate_spawn_env`,
/// `marker_list_has_no_duplicates`.
pub const INHERITED_SESSION_MARKERS: &[&str] = &[
    TRANSCRIPT_SUPPRESSING_MARKER,
    // The parent's session UUID. Inheriting it makes hooks and the statusline
    // attribute the child's events to the parent conversation.
    "CLAUDE_CODE_SESSION_ID",
    // "you are running inside Claude Code" marker. The spawned `claude` sets it
    // for its OWN children; inheriting it is the generic leak upstream 2.1.170
    // called out for the VS Code integrated terminal.
    "CLAUDECODE",
    // The parent interpreter's pid — stale by definition, and consulted by
    // shell-snapshot probes (`/proc/$CLAUDE_PID/comm`).
    "CLAUDE_PID",
    // A snapshot of the parent's CURRENT TURN effort level. It is derived
    // output exposed for hooks, not a user configuration knob, so a fresh
    // session must recompute it rather than adopt a stale turn's value.
    "CLAUDE_EFFORT",
    // Path to the parent's `claude` binary. tm always passes an absolute
    // resolved binary explicitly, so nothing in tm reads this.
    "CLAUDE_CODE_EXECPATH",
];

/// Environment variables a managed spawn sets DELIBERATELY — never scrub these.
///
/// Why (issue #4451 / #4455): this list exists to make over-scrubbing a
/// mechanical failure instead of a judgement call. `CLAUDE_CONFIG_DIR` is
/// relocated on purpose — it isolates the managed session's auth AND, since
/// #4455, is what puts the bundled agent roster in the `user` settings tier the
/// spawn's `--setting-sources user,project,local` flag loads. Scrub it and every
/// delegation silently degrades to `general-purpose` again. `CLAUDE_CODE_OAUTH_TOKEN`
/// is injected by #2246 to bypass the `CLAUDE_CONFIG_DIR`-keyed Keychain
/// divergence that otherwise produces a login loop.
///
/// Two further inherited variables are deliberately absent from
/// [`INHERITED_SESSION_MARKERS`] and are NOT scrubbed, because neither is
/// process-local state and neither affects transcript saving:
/// `CLAUDE_CODE_MAX_OUTPUT_TOKENS` is an operator-facing configuration knob
/// (scrubbing it would silently override the operator's intent), and
/// `CLAUDE_CODE_ENTRYPOINT` already carries the value a fresh CLI spawn would
/// compute for itself (`cli`).
/// What: the variable names a managed spawn assigns rather than unsets.
/// Test: `marker_list_is_free_of_deliberate_spawn_env`,
/// `config_dir_is_never_scrubbed`.
pub const DELIBERATE_SPAWN_ENV: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR,
];

/// The POSIX `env` unset flags that scrub every [`INHERITED_SESSION_MARKERS`]
/// entry.
///
/// Why: the tmux-pane spawn paths build a shell command STRING, so the scrub has
/// to be expressed as `env` flags rather than a `Command` mutation. Generating
/// them from the shared list keeps the string-command paths
/// (`spawn_command` / `resume_command`) and the `Command`-based paths
/// ([`scrub_command`]) from ever scrubbing different sets.
/// What: `" -u NAME"` per marker, each with a LEADING space so the result
/// concatenates directly onto an existing `env -u …` prefix. Marker names are
/// compile-time constants containing no shell metacharacters, so they need no
/// quoting. Every flag precedes any `NAME=VALUE` assignment, as POSIX `env`
/// grammar requires — an assignment before a `-u` makes `env` stop parsing
/// options and try to exec `-u` as a command.
/// Test: `env_unset_flags_covers_every_marker`,
/// `env_unset_flags_are_space_prefixed`.
pub fn env_unset_flags() -> String {
    INHERITED_SESSION_MARKERS
        .iter()
        .map(|name| format!(" -u {name}"))
        .collect()
}

/// Remove every [`INHERITED_SESSION_MARKERS`] entry from a [`std::process::Command`].
///
/// Why: the in-place relaunch (`build_inplace_exec_command`) and the
/// stream-json backend (`control::backend::stream_json`) exec `claude` through a
/// `Command` with no shell in between, so [`env_unset_flags`] does not apply to
/// them — but they leak exactly the same markers and need exactly the same
/// scrub. Using `env_remove` (rather than filtering an env map) keeps the
/// removal an explicit, inspectable part of the command that `get_envs()`
/// surfaces as `(key, None)`, which is what the tests assert on.
/// What: calls [`std::process::Command::env_remove`] once per marker.
/// Test: `scrub_command_removes_every_marker`,
/// `scrub_command_leaves_deliberate_env_intact`.
pub fn scrub_command(cmd: &mut std::process::Command) {
    for name in INHERITED_SESSION_MARKERS {
        cmd.env_remove(name);
    }
}

/// Parse the variable names an `env …` prefix string unsets via `-u NAME`.
///
/// Why: this is what lets the `tm doctor` probe read the scrub set out of the
/// REAL spawn command instead of restating the constant. A check that compared
/// [`INHERITED_SESSION_MARKERS`] against itself would be a tautology and would
/// stay green if someone dropped the flags from the spawn builder — the exact
/// shape of blind spot issue #4467 is about.
/// What: whitespace-splits `prefix` and collects the token following each
/// standalone `-u`. A trailing `-u` with no operand yields nothing.
/// Test: `parse_env_unset_vars_reads_the_real_spawn_prefix`,
/// `parse_env_unset_vars_ignores_assignments`,
/// `parse_env_unset_vars_tolerates_trailing_flag`.
pub fn parse_env_unset_vars(prefix: &str) -> Vec<&str> {
    let mut tokens = prefix.split_whitespace();
    let mut found = Vec::new();
    while let Some(token) = tokens.next() {
        if token == "-u"
            && let Some(name) = tokens.next()
        {
            found.push(name);
        }
    }
    found
}

/// Which [`INHERITED_SESSION_MARKERS`] are live in the CURRENT process env.
///
/// Why: `tm doctor` reports these so an operator can see whether this
/// invocation is itself running with the leak present — the difference between
/// "the scrub is configured correctly" (always checkable) and "the leak is
/// actually reaching me right now" (situational). Reported as context, never as
/// the pass/fail condition, because `tm doctor` run from a plain shell has no
/// markers set and the check must still be meaningful there.
/// What: delegates to [`markers_present_in`] with a real-environment lookup.
/// Test: covered via [`markers_present_in`].
pub fn markers_present_in_env() -> Vec<&'static str> {
    markers_present_in(|name| std::env::var_os(name).is_some())
}

/// Pure core of [`markers_present_in_env`]: which markers does `is_set` report?
///
/// Why: taking the lookup as a parameter keeps this testable without mutating
/// the process environment. `std::env::set_var` is `unsafe` in edition 2024 and
/// mutates state shared by every test in the binary, so a test that set a real
/// marker could flip the result of any concurrently-running test that reads the
/// environment. Injecting the lookup removes the shared mutable state instead of
/// trying to synchronise around it.
/// What: the subset of [`INHERITED_SESSION_MARKERS`] for which `is_set` returns
/// true. Presence is what matters, not value — Claude Code sets these to `"1"`
/// or an id, and an empty value would still have been inherited.
/// Test: `markers_present_in_detects_a_set_marker`,
/// `markers_present_in_is_empty_for_a_clean_env`,
/// `markers_present_in_reports_every_marker_when_all_are_set`.
fn markers_present_in<F>(is_set: F) -> Vec<&'static str>
where
    F: Fn(&str) -> bool,
{
    INHERITED_SESSION_MARKERS
        .iter()
        .copied()
        .filter(|name| is_set(name))
        .collect()
}

#[cfg(test)]
#[path = "claude_env_scrub_tests.rs"]
mod tests;
