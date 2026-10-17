---
name: tm-fleet-check
description: The Architect's supervision pass. Read the records, verify the observation channel, discover the live tmux sessions, read the poller's alerts and any push events, capture the panes that need it (a full poll when there are no events), answer high-confidence prompts, escalate the rest to the user, check the work against the goals, and update the records in `local/`. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, fleet, monitoring, tmux]
effort: high
---

# tm-fleet-check — One Supervision Pass

One pass of the Architect's supervision loop. Your Architect instructions set
the rules: the hard limits, the relay protocol, decisions as options and the
evidence labels. This skill is the checklist. This project's `CLAUDE.md` holds
the fleet specifics: the watch set, the user's name for the relay marker, and
the full-poll cadence. Read it before the first pass of a session.

Every path below is relative to this project's root, the directory that holds
its `CLAUDE.md`.

## Steps

1. **Read the records.** Read `local/instance.md`, `local/user.md`,
   `local/goals.md`, `local/state.md` and the tail of `local/actions.md`.
   They are the current facts; this skill does not repeat them.
2. **Discover, and verify the channel.** List the live sessions and panes
   with raw tmux, then cross-check the manager's inventory:

   ```sh
   tmux list-panes -a -F '#{session_name} #{pane_id} #{pane_current_command} #{pane_current_path}'
   tm ls --no-prune
   ```

   Verify the observation channel every pass; never assume the last pass's
   state still holds. When a tmux MCP server is loaded, cross-check its
   session list against the raw `tmux` output. When it reports fewer
   sessions, including none while tmux shows live ones, use the tmux CLI
   alone for this pass and note the fallback in `local/actions.md`.
   The manager and tmux are separate sources. The manager can omit a live
   session or keep a stopped one, and it never lists this session. Native
   tmux decides what runs. Note a disagreement in `local/actions.md`; it is
   not evidence that a session failed.
3. **Read the alerts.** The poller, `scripts/fleet-poll.py`, runs in the tmux
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
4. **Read the push events, when there are any.** The watched projects'
   notification hooks append `permission_prompt`, `idle_prompt` and
   `agent_needs_input` lines to `$ARCHITECT_INBOX_DIR/events.jsonl`
   (`inbox/events.jsonl` when the variable is unset). That file is absent
   until the hooks are installed (trusty-tools #8392), and absent is the
   normal case: this pass is then a full poll (step 5). When the file exists,
   read the lines whose `ts` is after `last_consumed_ts` in
   `local/state.md`, skip a line that has no `type` field, group the rest by
   `(project, tmux_session, tmux_pane)`, and log each by its `display` field.
   Once you have acted on the batch, advance `last_consumed_ts` to the newest
   `ts` you read (step 10). Never truncate or delete the file.
5. **Capture: targeted, with a full-poll fallback.**
   - Add one to `poll_count` in `local/state.md`.
   - Run a full poll when `events.jsonl` is absent, when it has no new lines
     this pass (an empty inbox), on every 4th pass (`poll_count % 4 == 0`),
     and on the first pass after this session started. A full poll captures
     the pane of every project in the watch set. It is the fallback for a
     missed event: a hook not yet installed, a hook that failed silently, or
     a project launched outside the watch set. Events narrow the common
     case; they never replace the raw capture.
   - Otherwise capture only the panes the alerts and events named.
   - Capture a bounded tail with an exact target:
     `tmux capture-pane -p -t '=<session>:' -S -80`.
   - Record the mode (targeted or full poll) and why in `local/actions.md`.
   - Look for the user's own turns: a submitted `❯` line without your relay
     marker, or a dialog answer you did not select. Each is a user ruling.
     Record it, skip any question it already answered, and never relay a
     second copy.
6. **Classify confidence.** Answer a PM directly only at high confidence: a
   clear current prompt, an answer backed by an explicit user ruling or an
   established project decision, and no open ambiguity with consequences.
   Everything else goes to the user with the evidence and a recommendation,
   prefixed `[<tmux-session>]:`. Keep the other projects moving while an
   answer is pending.
7. **Never touch a draft.** Re-capture right before any send, and classify
   the input box:

   ```sh
   tm architect input-state '=<session>:'
   ```

   It prints `<target> <state> | <text>`. Send only when the state is `empty`
   or `suggestion` (Claude Code's dim auto-suggestion). `typed` is a draft
   that belongs to the user or another agent; `no-prompt` means a turn or a
   dialog is running. Wait for the next pass in both cases. `missing`, with
   exit status 1, means the pane could not be captured: send nothing.
8. **Send with the two-step pattern**, never in one call.
   `scripts/relay.sh <session> '<text>'` types the text, checks the box and
   presses Enter only when both checks pass; then capture to confirm:

   ```sh
   scripts/relay.sh <session> '[<Name> Architect HH:MMZ] Architect answer: … (basis: …)'
   tmux capture-pane -p -t '=<session>:' -S -20    # a submitted turn and an acknowledgement
   ```

   By hand, the same steps are:

   ```sh
   tmux send-keys -t '=<session>:' -l '[<Name> Architect HH:MMZ] Architect answer: … (basis: …)'
   tm architect input-state '=<session>:'    # typed | <the start of your text>
   tmux send-keys -t '=<session>:' Enter
   ```

   Press Enter only when the state is `typed` and your message starts with
   the printed text. The command prints only the `❯` row, cut at 100
   characters, so a message that wraps shows its first row alone. Otherwise
   do not press Enter; record it and check again next pass. Before typing
   into a session another Architect also watches, run
   `scripts/posted.sh <session> <HH:MMZ the question appeared>` and send only
   on `NONE`.
   `HH:MMZ` comes from `date -u`. When the fleet runs on a non-default tmux
   server, add `-S "$TMUX_SOCKET"` to each `tmux` call and export
   `TMUX_SOCKET` for `tm architect input-state` and the scripts.
9. **Check context and goals.**
   - A session at the context bar (250k tokens by default; set in
     `CLAUDE.md`) needs a refresh. The poller's `ctx` alert says
     `approaching` at 90% of the bar and `bar` at the bar. Run
     `tm-context-refresh` at a safe boundary. Never force it over active work or a draft: queue it in
     `local/state.md` and record the deferral. A `self_ctx` alert is your own
     context; refresh yourself the same way at a quiet moment.
   - Map each live session and its current work to a goal in
     `local/goals.md`. Compare capacity (load against cores, builder slots,
     disk) with the goal order, and pace a lower-ranked PM when the top goal
     needs room. Order the pending questions by goal rank. On a full-poll
     pass, add any optimization proposal and flag drift from the goals.
10. **Update the records before you end the pass.**
    - `local/projects/<project>.md`: what each project is doing, with every
      fact labelled reported or verified. A new project starts from
      `local/projects/_template.md`.
    - `local/state.md`: `poll_count`, `last_consumed_ts`, `last_alert_ts`,
      the pending questions and the context-refresh queue.
    - `local/goals.md`: the session-to-goal map and any proposal.
    - `local/instance.md`: an instance fact you found or corrected.
    - `local/actions.md`: one entry stamped with `date -u`: the mode, what
      you sent and where, and what is still pending.
    - Commit them to this project's local repository:
      `git add local && git commit -q -m "local: pass HH:MMZ"`. The
      repository has no remote; never add one.
    - Never write a secret or a raw terminal dump into a record.
11. **Notify only on signal.** A quiet, unchanged pass needs no message to
    the user. Report meaningful change, completion, failure, or a decision
    the user owes, and never re-ask a pending question.

## Anti-patterns

- Treating one source, a tmux MCP server, the poller or `tm ls`, as ground
  truth without the raw `tmux` check.
- Restarting or killing a session because the manager reports it errored
  while tmux shows a live pane.
- Re-asking a question that is escalated and still unanswered.
- Sending a checkpoint command into a pane that holds unsubmitted input.

## Related skills

- `tm-context-refresh`: the pause, clear and resume procedure step 9 starts.
- `architect-tmux-ops`: capture, classify and send.
- `architect-operating-rules`: the relay format and the decisions.
- `tm-architect-setup`: how this project and its poller were set up.
