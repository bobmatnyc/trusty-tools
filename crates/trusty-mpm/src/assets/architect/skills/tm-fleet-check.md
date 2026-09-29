---
name: tm-fleet-check
description: The Architect's supervision pass. Discover the live tmux sessions, read the poller's alerts and any push events, capture the panes that need it (a full poll when there are no events), answer high-confidence prompts, escalate the rest to the user, and update the records in `records/`. Architect only.
user-invocable: true
version: "0.1.0"
category: monitoring
tags: [architect, supervisor, fleet, monitoring, tmux]
effort: high
---

# tm-fleet-check — One Supervision Pass

One pass of the Architect's supervision loop. Your supervisor instructions set
the rules: the hard limits, the relay protocol, decisions as options and the
evidence labels. This skill is the checklist. This project's `CLAUDE.md` holds
the fleet specifics: the watch set, the user's name for the relay marker, and
the full-poll cadence. Read it before the first pass of a session.

Every path below is relative to this project's root, the directory that holds
its `CLAUDE.md`.

## Steps

1. **Discover.** List the live sessions and panes with raw tmux, then
   cross-check the manager's inventory:

   ```sh
   tmux list-panes -a -F '#{session_name} #{pane_id} #{pane_current_command} #{pane_current_path}'
   tm ls --no-prune
   ```

   The two are separate sources. The manager can omit a live session or keep
   a stopped one, and it never lists `tm-architect`. Native tmux decides what
   runs. Note a disagreement in `records/actions.md`; it is not evidence that
   a session failed.
2. **Read the alerts.** The poller, `scripts/fleet-poll.py`, runs in tmux
   session `tm-architect-poll`. It appends each new alert as one JSON line to
   `inbox/alerts.jsonl` and types a one-line pointer into this pane. Read the
   lines newer than `last_alert_ts` in `records/state.md`; each line's
   `display` field already starts with `[<tmux-session>]:`. Check that the
   poller runs with `tmux has-session -t =tm-architect-poll`. When it does
   not, report that and record it. Start it with `scripts/start-fleet-poll.sh`
   only when the user asks: a stopped monitor is never restarted unasked.
3. **Read the push events, when there are any.** The watched projects'
   notification hooks append `permission_prompt`, `idle_prompt` and
   `agent_needs_input` lines to `$ARCHITECT_INBOX_DIR/events.jsonl`
   (`inbox/events.jsonl` when the variable is unset). That file is absent
   until the hooks are installed (trusty-tools #8392), and absent is the
   normal case: this pass is then a full poll (step 4). When the file exists,
   read the lines whose `ts` is after `last_consumed_ts` in
   `records/state.md`, group them by `(project, tmux_session, tmux_pane)`,
   and log each by its `display` field. Never truncate or delete the file.
4. **Capture: targeted, with a full-poll fallback.**
   - Add one to `poll_count` in `records/state.md`.
   - Run a full poll when `events.jsonl` is absent, when it has no new lines
     this pass (an empty inbox), on every 4th pass (`poll_count % 4 == 0`),
     and on the first pass after this session started. A full poll captures
     the pane of every project in the watch set.
   - Otherwise capture only the panes the alerts and events named.
   - Capture a bounded tail with an exact target:
     `tmux capture-pane -p -t '=<session>:' -S -80`.
   - Record the mode (targeted or full poll) and why in `records/actions.md`.
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
   python3 scripts/input-state.py '=<session>:'
   ```

   It prints `<target> <state> | <text>`. Send only when the state is `empty`
   or `suggestion` (Claude Code's dim auto-suggestion). `typed` is a draft
   that belongs to the user or another agent; `no-prompt` means a turn or a
   dialog is running. Wait for the next pass in both cases.
7. **Send with the two-step pattern**, never in one call:

   ```sh
   tmux send-keys -t '=<session>:' -l '[<Name> supervisor HH:MMZ] Supervisor answer: … (basis: …)'
   python3 scripts/input-state.py '=<session>:'    # typed | <the start of your text>
   tmux send-keys -t '=<session>:' Enter
   tmux capture-pane -p -t '=<session>:' -S -20    # a submitted turn and an acknowledgement
   ```

   Press Enter only when the state is `typed` and your message starts with
   the printed text. The script prints only the `❯` row, cut at 100
   characters, so a message that wraps shows its first row alone. Otherwise
   do not press Enter; record it and check again next pass.
   `HH:MMZ` comes from `date -u`. When the fleet runs on a non-default tmux
   server, add `-S "$TMUX_SOCKET"` to each `tmux` call and export
   `TMUX_SOCKET` for `scripts/input-state.py`.
8. **Check context.** A pane whose status line shows `ctx` at or above 50%
   needs a refresh; 45 to 49% is approaching. 50% is the default; this
   project's `CLAUDE.md` may set another threshold. Run `tm-context-refresh`
   at a safe boundary. Never force it over active work or a draft: queue it
   in `records/state.md` and record the deferral. A `self_ctx` alert is your
   own context; refresh yourself the same way at a quiet moment.
9. **Update the records before you end the pass.**
   - `records/projects/<project>.md`: what each project is doing, with
     every fact labelled reported or verified.
   - `records/state.md`: `poll_count`, `last_consumed_ts`, `last_alert_ts`,
     the pending questions and the context-refresh queue.
   - `records/actions.md`: one entry stamped with `date -u`: the mode, what
     you sent and where, and what is still pending.
   - Commit them to this project's local repository:
     `git add records && git commit -q -m "records: pass HH:MMZ"`. The
     repository has no remote; never add one.
   - Never write a secret or a raw terminal dump into a record.
10. **Notify only on signal.** A quiet, unchanged pass needs no message to
    the user. Report meaningful change, completion, failure, or a decision
    the user owes, and never re-ask a pending question.

## Anti-patterns

- Treating one source, the poller or `tm ls`, as ground truth without the raw
  `tmux` check.
- Restarting or killing a session because the manager reports it errored
  while tmux shows a live pane.
- Re-asking a question that is escalated and still unanswered.
- Sending a checkpoint command into a pane that holds unsubmitted input.

## Related skills

- `tm-context-refresh`: the pause, clear and resume procedure step 8 starts.
- `tm-supervisor-setup`: how this project and its poller were set up.
