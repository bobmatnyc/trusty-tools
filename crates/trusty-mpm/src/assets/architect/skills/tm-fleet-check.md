---
name: tm-fleet-check
description: The Architect's supervision pass. Verify the tmux observation channel, read the poller's alerts and any push events, capture the panes that need it (a full poll when there are no events), verify PM claims, answer high-confidence prompts, escalate the rest to the user, check goals and capacity, and update the records in `local/`. Architect only.
user-invocable: true
version: "0.2.0"
category: monitoring
tags: [architect, fleet, monitoring, tmux]
effort: high
---

# tm-fleet-check — One Supervision Pass

One pass of the Architect's supervision loop. Your Architect instructions
(architect.md, composed into your instructions by tm) set the rules: the hard
limits, the relay protocol, decisions as options and the evidence labels. This
skill is the checklist. `AGENTS.md` holds the instance facts: the user's name
for the relay marker, the host and the fleet. `local/state.md` holds the watch
set. Load `architect-tmux-ops` before step 1 and before any capture or send.

Every path below is relative to this project's root, the directory that holds
its `AGENTS.md`.

## Steps

1. **Verify the observation channel, every pass.** Never assume last pass's
   working or broken state still holds. List the live sessions and panes with
   raw tmux, then cross-check the manager's inventory:

   ```sh
   tmux list-panes -a -F '#{session_name} #{pane_id} #{pane_current_command} #{pane_current_path}'
   tm ls --no-prune
   ```

   The two are separate sources. The manager can omit a live session or keep
   a stopped one, and it never lists this session. Native tmux decides what
   runs. When an MCP tmux tool is loaded and under-reports against raw tmux,
   use the tmux CLI alone for this pass. Note a disagreement in
   `local/actions.md`; it is not evidence that a session failed.
2. **Read the alerts.** The poller, `scripts/fleet-poll.py`, runs in the tmux
   session named after this one plus `-poll`. It appends each new alert as one
   JSON line to `inbox/alerts.jsonl` and types a one-line pointer into this
   pane. Read the lines newer than `last_alert_ts` in `local/state.md`; each
   line's `display` field already starts with `[<tmux-session>]:`. Check that
   the poller runs, reading this session's name from tmux, never assuming it:

   ```sh
   tmux has-session -t "=$(tmux display-message -p '#S')-poll"
   ```

   `tm fleet status --json` names the same session in its `session` field.
   When the poller does not run, report that and record it. Start it with
   `scripts/start-fleet-poll.sh` only when the user asks: a stopped monitor
   is never restarted unasked.
3. **Read the push events, when there are any.** The watched projects'
   notification hooks append `permission_prompt`, `idle_prompt` and
   `agent_needs_input` lines to `$ARCHITECT_INBOX_DIR/events.jsonl`
   (`inbox/events.jsonl` when the variable is unset). That file is absent
   until the hooks are installed (trusty-tools #8392), and absent is the
   normal case: this pass is then a full poll (step 4). When the file exists,
   read the lines whose `ts` is after `last_consumed_ts` in
   `local/state.md`, group them by `(project, tmux_session, tmux_pane)`,
   and log each by its `display` field. Lines with `kind` and no `type` are
   extractor records; skip them in the grouping. Never truncate or delete the
   file.
   **PM-message claims.** When a PM message claims a merge, close, deploy,
   publish, CI run or head SHA, write one event JSON (schema in the
   `scripts/verify-claim.py` docstring; unknown values null, SHAs copied
   verbatim) and run `scripts/verify-claim.py '<json>'`. The claim is verified
   only on its MATCH line; a MISMATCH goes back to the PM; UNCHECKED stays
   reported. For a batch of three or more messages, or one over about 3k
   tokens, dispatch `event-extractor` once instead.
4. **Capture: targeted, with a full-poll fallback.**
   - Add one to `poll_count` in `local/state.md`.
   - Run a full poll when `events.jsonl` is absent, when it has no new lines
     this pass (an empty inbox), on every 4th pass (`poll_count % 4 == 0`),
     and on the first pass after this session started. A full poll captures
     the pane of every project in the watch set.
   - Otherwise capture only the panes the alerts and events named.
   - Capture a bounded tail with an exact target:
     `tmux capture-pane -p -t '=<session>:' -S -80`.
   - Record the mode (targeted or full poll) and why in `local/actions.md`.
   - Look for the user's own turns: a submitted `❯` line without your relay
     marker, or a dialog answer you did not select. Each is a user ruling.
     Record it, skip any question it already answered, and never relay a
     second copy.
5. **Classify confidence.** Answer a PM directly only at high confidence: a
   clear current prompt, and an answer backed by an explicit user ruling or an
   established project decision. Everything else goes to the user with the
   evidence and a recommendation, prefixed `[<tmux-session>]:`. Keep the
   other projects moving while an answer is pending.
6. **Never touch a draft.** Re-capture right before any send, and classify
   the input box:

   ```sh
   tm architect input-state '=<session>:'
   ```

   It prints `<target> <state> | <text>`. Send only when the state is `empty`
   or `suggestion` (Claude Code's dim auto-suggestion). `typed` is a draft
   that belongs to the user or another agent; `no-prompt` means a turn or a
   dialog is running. Wait for the next pass in both cases. `missing`, with
   exit status 1, means the pane could not be captured: send nothing.
7. **Send with the two-step pattern**, never in one call:

   ```sh
   tmux send-keys -t '=<session>:' -l '[<Name> Architect HH:MMZ] Architect answer: … (basis: …)'
   tm architect input-state '=<session>:'    # typed | <the start of your text>
   tmux send-keys -t '=<session>:' Enter
   tmux capture-pane -p -t '=<session>:' -S -20    # a submitted turn and an acknowledgement
   ```

   Press Enter only when the state is `typed` and your message starts with
   the printed text. The command prints only the `❯` row, cut at 100
   characters, so a message that wraps shows its first row alone. Otherwise
   do not press Enter; record it and check again next pass.
   `HH:MMZ` comes from `date -u`. When the fleet runs on a non-default tmux
   server, add `-S "$TMUX_SOCKET"` to each `tmux` call and export
   `TMUX_SOCKET` for `tm architect input-state`.
8. **Check context.** Keep watched PMs below about 250k tokens of context;
   220k to 249k is approaching. Run `tm-context-refresh` at a safe boundary.
   Never force it over active work or a draft: queue it in `local/state.md`
   and record the deferral. A `self_ctx` alert is your own context; refresh
   yourself the same way at a quiet moment.
9. **Check goals and capacity.** Read `local/goals.md`. Map each live
   session and its work to a goal. Compare capacity (load against cores,
   builder slots, disk) with the goal order, and pace lower-ranked PMs when
   the top goal needs room. Order pending questions by goal rank. Run a board
   sweep when one is due (start of day, after a restart, a PM with an empty
   queue). On a full-poll pass, add any optimization proposal and name drift.
   Update `local/goals.md` (details: `architect-strategy`).
10. **Update the records before you end the pass.**
    - Each project's state goes to that project's memory palace with
      `memory_remember`, every fact labelled reported or verified.
    - `local/state.md`: `poll_count`, `last_consumed_ts`, `last_alert_ts`,
      the pending questions and the context-refresh queue.
    - `local/actions.md`: one entry stamped with `date -u`: the mode, what
      you sent and where, and what is still pending.
    - Write the changed sessions, goals and owner items to the dashboard in
      one batch (`architect-dashboard`).
    - Commit them to this project's local repository:
      `git add local && git commit -q -m "local: pass HH:MMZ"`. The
      repository has no remote; never add one.
    - Never write a secret or a raw terminal dump into a record.
11. **Notify only on signal.** A quiet, unchanged pass needs no message to
    the user. Report meaningful change, completion, failure, or a decision
    the user owes, and never re-ask a pending question. Messages to the user
    use the voice guide (`docs/architect/voice.md`); relays to PMs and every
    record stay plain.

## Anti-patterns

- Treating one source, the poller, an MCP tool or `tm ls`, as ground truth
  without the raw `tmux` check.
- Restarting or killing a session because the manager reports it errored
  while tmux shows a live pane.
- Re-asking a question that is escalated and still unanswered.
- Sending a checkpoint command into a pane that holds unsubmitted input.

## Related skills

- `architect-tmux-ops`: the tmux read and send commands this pass uses.
- `tm-context-refresh`: the pause, clear and resume procedure step 8 starts.
- `tm-fleet-restart`: a relaunch, resume or Remote Control restore.
- `tm-architect-setup`: how this project and its poller were set up.
