# #10459 df-G04 — one main-thread ticket for (A) Architect set/copy and (B) 1-step direct commands

Design draft for the Architect. No code. Rev 3, 2026-10-30: owner rulings Dac and Daf, PR0
spike verdict, design-critic BLOCK items applied.

**Scope: the Architect session only.** Tickets, turn grants and `/tm-run` exist only in the
session `architect_main_thread` binds. PM sessions and every agent keep today's rules
exactly: their hooks mint nothing, and the daemon judges them as now.

**Known limit (Daf):** any code running as the Architect's uid can forge provenance. No
human confirm ships. See §1.

Paths: `MC` = `crates/trusty-mpm/src/bin/tm/commands/`, `MCORE` = `crates/trusty-mpm/src/core/`,
`TS` = `crates/trusty-secrets/src/`, `AS` = `crates/trusty-mpm/src/assets/architect/scripts/`.

## 0. Facts this rests on (verified)

- Hook payloads (https://code.claude.com/docs/en/hooks). Common fields: `session_id`,
  `prompt_id`, `transcript_path` (async), `cwd`, `permission_mode`, `hook_event_name`.
  `agent_id` is present inside a subagent or teammate; **`agent_id` absent means the main
  thread**. `agent_type` is not a subagent signal: it also appears on the main thread when
  the session runs with `--agent`. PreToolUse adds `tool_name`, `tool_input`, `tool_use_id`;
  output `permissionDecision` is `allow|deny|ask|defer`. `MC/pm_guard_cost.rs:9` lists
  `prompt_id` on PreToolUse; the "no turn_id" note in `MC/pm_guard_budget/mod.rs:10-14` is stale.
- UserPromptSubmit carries `prompt`. Pasted text is delimited by `<pasted_content id=…>`
  only "where Claude Code marks pasted text"; short pastes (≤800 chars and ≤3 lines) are not
  marked. It also fires for scheduled tasks, `/loop`, background-subagent reports and
  cross-session messages, with no origin field. `UserPromptExpansion` fires "when a
  user-typed command expands into a prompt", matcher on the command name, can block.
- `onFailure: "block"` on a `command` hook makes a failure (cannot start, exit ≠0/2,
  timeout, invalid output) act as exit 2; requires Claude Code **v2.1.295**. Unset, a
  timed-out PreToolUse hook lets the call through.
- `crossSessionInbound` (`accept|hold|refuse`, cross-session messaging since v2.1.224)
  controls inbound messages from the operator's other sessions.
- Exec-form hooks spawn with no shell; pm-guard is shell form today
  (`MCORE/session_launch/settings.rs:774`).
- `MCORE/twin_identity.rs:188` `thread_kind`; `MC/pm_guard_architect_reason.rs:95`
  `architect_main_thread`; `MCORE/architect_launch.rs` launch records.
- trusty-secrets judges the socket peer by pid (`TS/server/router.rs:466`) and ancestry
  (`TS/ancestry.rs`). P1: `fix/10459-architect-set-copy` @`cf080e9f2f`
  (`TS/server/architect.rs`, audit fields `architect`, `architect_refusal`).
- Pane floor #8902 (`MC/pm_guard_bash/architect_pane*.rs`, verbs at
  `architect_pane_verbs.rs:38`) denies non-Architect callers input/kill/move verbs at an
  Architect pane; the main thread is exempt (`MC/pm_guard_floor.rs:131`).
- Pane senders: `AS/fleet-poll.py:191-196` `send_to_architect` types the `[poll HH:MMZ]`
  wake into the Architect pane, and `:368` sends a bare `Enter` after confirming the text;
  `AS/self-clear.sh` sends `/clear` and a fixed resume prompt into it; `AS/relay.sh:66,86`
  targets PM panes.

## 1. Known limit (for the spec and the PR body)

**PR0 spike verdict: NO.** No hook channel resists same-uid forgery without root-owned
managed config. Evidence: HTTP hook settings hot-reload; an `mcp_tool` hook's config or
binary can be replaced; hooks inherit the environment (hooks.md:752); a sibling process
reads env with `ps -E`. Also: tmux can type into the pane and answer an `ask` dialog.

**Not guaranteed.** Any code running as the Architect's uid — an MCP server, a background
child, a script, an interpreter, an edited settings file — can forge a mint, type a
`/tm-run` prompt into the pane, answer an `ask`, or redeem a ticket in its window. A
redeemed ticket proves a mint happened, not who caused it.

**Guaranteed.** Subagents and PM sessions cannot mint through hooked paths: their tool
calls carry `agent_id` or lack the Architect binding, and pm-guard denies every hooked
route to the mint (hook verb, ticket methods, pane input, cross-session marker). Every
mint, redeem, void and refusal is audited before it takes effect. Values are never
printed. Limit on this guarantee: any agent able to run a program reaches unhooked code
(the program itself), so it covers only commands pm-guard can read.

**Hardening path (future, no build).** Root-owned managed config: `allowManagedHooksOnly`
plus `managedMcpServers`, root-owned hook and daemon binaries, and the daemon performs the
action itself instead of honouring a redeemer. MCP confirm carrier
`_meta["anthropic/requiresUserInteraction"]` as a per-call human step.

## 2. Mechanism: hook-minted tickets

**Mint checks** (daemon; each fail-closed): peer is the installed `tm` (realpath in the
cargo bin dir, not group/world writable); argv is the registered hook argv; parent is a
`claude` process (exec form); peer started ≤2 s ago; that `claude` has a live launch
record. pm-guard denies any Bash call naming the hook verb or ticket methods.

**What the launch record binds:** that `claude` (pid, start time) is the process `tm fleet
init`/`relaunch` started in the Architect project. It keeps a PM's or nested session's
hook from minting. It is a same-uid file, so it is not an independent factor (§1).

**Ticket** (daemon memory only): `{ticket_id, session_id, prompt_id, tool_use_id,
claude_pid, claude_start, kind: secrets_op|direct_cmd, argv_sha256, op, project, key_name,
minted_at}`. No separate active state: a ticket is live from mint until voided.

**Window.** The mint returns `ask`. A redeemer counts only if it started after the mint,
and Claude Code starts the Bash process only once the user approves, so in practice
redemption opens at approval. Voided by: first redemption; **every** PreToolUse in the
session, subagents and teammates included, and the session's Stop (each sends
`secrets.ticket.void(session)`, a no-op when nothing is pending); a decline (the next event
voids it); TTL 60 s. A main-thread-only void would be HIGH: a subagent's call would leave
the ticket open. **Usability cost:** a background agent's tool call during the dialog
voids the ticket before approval; the user re-issues the command. **Remaining window:**
from mint to the next hook event, any process started after mint under that `claude` with
the same argv can redeem — only unhooked same-uid code, under §1.

**Minting switch:** `mint_enabled` in the trusty-secrets daemon settings
(`TS/server/settings.rs`), default `false` from PR 1; PR 5 sets it `true` for the
Architect project. While `false`, `secrets.ticket.mint` refuses and is audited.

**Redemption:** nearest-`claude` (pid, start), argv hash, peer start after mint,
op/project/key, not voided. Consumed atomically under the key's index lock.

## 3. How (A) and (B) share it

**(A)** PreToolUse, Architect main thread, one parseable `tm secrets set|copy` argv (no
pipes, chains, expansions, `xargs`) → mint `secrets_op`, return `ask`. The daemon honours
P1's record only with a redeemed ticket. Gated by Q2 (§7).

**(B)** behind `[architect] direct_commands = false` (default off). `/tm-run` is a
registered command with `disable-model-invocation: true`, so the model cannot invoke it.
The grant is minted on **UserPromptExpansion** (matcher `tm-run`), not on UserPromptSubmit,
and only when UserPromptSubmit for the same `prompt_id` saw a prompt that is exactly that
one line. Refused: `<pasted_content` or `<system-reminder` text, a second line, >1 KiB, a
`prompt_id` already granted, a pending ticket. PreToolUse, main thread, same `prompt_id`,
one Bash call with the granted argv hash → skips checks it would fail, audited, `ask`,
`direct_cmd` ticket if a secrets verb. Never eligible: value-printing commands,
trust-anchor writes (Q5).

Evaluation of the expansion route: it removes model invocation and plain-prompt text that
merely contains the marker. It does not separate typed from tmux-typed or short-pasted
input; both expand. **Residual under §1:** a short paste (≤800 chars and ≤3 lines) is
unmarked, so a one-line `/tm-run X` pasted by the user counts as typed.

**Scheduled input.** pm-guard denies `CronCreate` and `ScheduleWakeup` calls, and `/loop`
prompts, whose body contains `/tm-run`, in the Architect session. Pick one at PR 3: the
deny, or `CLAUDE_CODE_DISABLE_CRON=1` in the Architect's launch env. Test either way.
Second layer: `disable-model-invocation: true` also blocks a scheduled task from firing
the command (skills.md:402). A cross-session message's slash commands do not run as
commands, so a relayed `/tm-run` never expands.

## 4. Gap: tmux send-keys into the Architect pane (owner, verbatim)

"Gap: tmux send-keys into the Architect pane also fires it, so the design must block or
tag that." tmux keys are terminal input; nothing downstream can tell them from the user.

**Recommendation: (a) block.** (b) tag is not possible: terminal input carries no tag, and
a ledger of allowed sends would cover only hooked senders, which (a) already denies.

- **a1, PMs and all agents** (Architect subagents included): #8902 already denies every
  deny-set verb. Add tty writes: `>`, `>>`, `tee`, `dd of=`, `cp` to `#{pane_tty}`, or to
  any `/dev/ttys*` / `/dev/pts/*` the guard cannot place while an Architect is live.
- **a2, the Architect main thread:** full deny of every direct `send-keys`, `send-prefix`,
  `paste-buffer`, `pipe-pane`, and `set-buffer`/`load-buffer` followed by a paste, to its own
  pane. The exemption at `MC/pm_guard_floor.rs:131` no longer covers these verbs.
- **Pinned senders** are the only ones: `AS/self-clear.sh` and `AS/fleet-poll.py`
  `send_to_architect`, invoked by exact path. The installed scripts directory joins the
  protected paths, so a hooked edit is denied. Each pinned sender asks `tm architect
  ticket-pending` before every send, the bare `Enter` at `fleet-poll.py:368` included, and
  skips while a ticket is pending or the query fails. None of their fixed strings
  starts with `/tm-run`, so none mints a grant; a test pins that.
- **a3, cross-session senders:** deny any hooked `tm session send`, `SendMessage`,
  `memory_send_message` or `send-keys` relay (`crates/trusty-mpm/src/daemon/action_events/tmux_send.rs`)
  whose body contains `/tm-run`, from every session to every target.
- **Inbound:** pin `crossSessionInbound: "refuse"` in the Architect's tm-written `--settings` or project settings.
  The fleet reaches the Architect through the poll wake and the memory inbox, not native
  cross-session messages; confirm no flow needs native inbound before PR 3, else `hold`.

**Target identification** (reuse `architect_pane_probe.rs`, `tmux_exact_target.rs`): list
`#{pane_id} #{window_id} #{session_id} #{pane_pid} #{session_name} #{pane_tty}` on the
server the call selects (`-L`/`-S`, `TMUX`); a pane is the Architect's when its pid is in
`live_architect_lineage`, its session is `tm-architect`, or a `*.architect-session` sidecar
names it. Resolve `-t` as tmux does: `%N`, `@N`, `$N`, `=name:`, `name:win.pane`, prefix
and fnmatch matches, linked windows, session groups. No `-t` = the caller's pane; unknown
denies. Expansions, globs, `{marked}`, `~`, `!`, `+1`, `#{…}` deny while an Architect is
live. The listing shares the 2 s budget; a timeout denies.

**Bypass residue (all under §1):** unhooked code running tmux or answering `ask`; #8902's
residuals (`command-alias`, tmux config, `watch`, `script`, `su -c`, `ssh`, interpreters,
keys in pieces); TIOCSTI from an interpreter; an unhooked edit of a pinned script; the
redemption window in §2.

## 5. Hook failure

Only the Architect's PreToolUse hook sets `onFailure: "block"`. The UserPromptSubmit and
UserPromptExpansion hooks are registered only when `direct_commands = true`; a failure
there mints no grant, which is already closed. `tm launch` refuses the Architect on Claude
Code below v2.1.295 (pinned minimum). A settings golden test pins the exact hook JSON for
both flag values. A live check registers a hook that sleeps past its `timeout` and proves
the call is blocked. A failed `secrets.ticket.void` does NOT deny the call: the daemon
holds the tickets, so the 60 s TTL bounds an unvoided one.

## 6. Fail-closed rules and audit

Deny, keeping today's rules, on: daemon unreachable; any mint check failure; missing
`session_id`/`prompt_id`; `agent_id` present or malformed; unparseable argv; audit log
unwritable (write-ahead, `TS/server/gate.rs`); clock or process-table error; ticket field
mismatch; pane listing error; PreToolUse hook failure (§5). Exception: a failed void (§5).

Audit record per mint, redeem, void, refuse: `{ts, event, kind, ticket_id,
session_id, prompt_id, tool_use_id, claude_pid, op, project, key_name, argv_sha256,
marker, decision, reason_arm, void_cause, architect, architect_refusal}`. No value, no
env, no argument beyond the key name. P1's tree-wide exemption is never merged alone (Q4).

## 7. Owner answers

- **Q1** dropped (Daf): no confirm; §1 is the known limit.
- **Q2 yes.** Verify that `ask` prompts under `bypassPermissions` (PR 4). It gates (A) and
  (B): if `ask` does not prompt, neither is enabled.
- **Q3 yes.** Spike done (PR0, verdict in §1); transcript origin fields are not relied on.
- **Q4 yes.** The launch record stays required at mint and redemption.
- **Q5 yes.** (B) never relaxes the trust-anchor floor or value-print rules; one simple argv.

## 8. Build plan

Order: floor and Q2 check land before either verb is enabled; (B) ships default-off.

| PR | Crate | Rung | Content | Tests proving the fail-closed arms |
|---|---|---|---|---|
| 1 | trusty-secrets | 5 | Rebase P1 (`cf080e9f2f`); in-memory ticket store, `secrets.ticket.mint/void/pending`, mint checks and voids, redemption in `set` and local `copy`; P1's record check required with a ticket; `mint_enabled = false` until PR 5 | keep `architect_refusals_each_keep_the_agent_rules`, `nested_claude_under_the_architect_is_not_the_architect`, `architect_with_an_unreadable_process_chain_is_refused`, both bridge tests; `architect_with_a_valid_record_sets_a_denied_key` needs a ticket; add `record_without_ticket_keeps_agent_rules`, `ticket_without_record_is_refused`, `mint_from_a_non_tm_exe`/`_a_grandchild`/`_an_old_process_is_refused`, `ticket_replay`/`_argv_mismatch`/`_peer_started_before_mint_is_refused`, `next_event_voids_a_pending_ticket`, `subagent_pretooluse_voids_pending_ticket`, `mint_refused_while_mint_disabled`, `decline_then_stop_voids`, `ttl_expiry_voids`, `second_mint_voids_both`, `audit_unwritable_at_mint`/`_at_redeem`/`_at_void_refuses`, `process_table_error_at_mint_refuses`, `clock_error_refuses`, `restart_drops_tickets` |
| 2 | trusty-mpm | 5 | Exec form, `onFailure: "block"` on PreToolUse only, minimum version check (§5) | `architect_hook_settings_golden` (both `direct_commands` values; prompt hooks absent when `false`), `pm_session_registers_no_ticket_handler`, `launch_refuses_below_minimum_version`; live: sleeping hook blocks the call |
| 3 | trusty-mpm | 5 | Pane floor a1–a3, pinned senders with `ticket-pending`, protected scripts dir, cron/loop marker deny, `crossSessionInbound` pin | extend `every_deny_verb_is_denied_in_each_target_form`; `main_thread_send_keys_to_own_pane_is_denied`, `set_buffer_then_paste_is_denied`, `pane_tty_write_is_denied`, `relay_body_with_marker_is_denied`, `cron_and_wakeup_with_marker_are_denied`, `pinned_senders_skip_while_pending`, `architect_settings_pin_cross_session_refuse`, `pending_query_failure_skips_send`, `pinned_strings_never_carry_the_marker`, `pane_listing_timeout_denies`, `script_dir_write_is_denied` |
| 4 | trusty-mpm | 5 | Q2 live check via `scripts/sandbox_daemon.sh` + `scripts/sandbox_secrets_console.sh` | `ask` shown under `bypassPermissions`; result recorded here |
| 5 | trusty-mpm (+4 consumer) | 5 | (A) mint enabled; Bash deny of hook verb and ticket methods | `bash_naming_hook_verb_or_ticket_method_is_denied` (incl. `exec tm hook --pm-guard` from the main thread and from a subagent), `failed_void_does_not_deny_the_call`, `subagent_call_never_mints`, `pm_session_never_mints`, `agent_type_alone_is_still_main`, `malformed_agent_id_refuses`, `missing_prompt_id_refuses`, `chained_argv_gets_no_ticket`, `daemon_down_at_mint_keeps_todays_rules` |
| 6 | trusty-mpm | 5 | (B) behind `direct_commands`, `/tm-run` command, UserPromptExpansion grant | `flag_off_mints_no_grant`, `model_invocation_of_tm_run_is_refused`, `pasted_or_reminder_prompt_gets_no_grant`, `second_line_gets_no_grant`, `value_print_command_is_never_direct`, `expansion_without_matching_submit_gets_no_grant`, `expansion_without_prompt_id_gets_no_grant`; live: UserPromptExpansion carries `prompt_id` (absent → no grant, fail closed) |

Each PR carries a changelog fragment in the crate it touches.
