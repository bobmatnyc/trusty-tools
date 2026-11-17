# State

The Architect's running state, rewritten in place at the end of every pass.
History goes in `local/actions.md`.

- task: Waiting for the owner's #12 Q40/Q41 answer, relayed by the reviewer (02:00Z); D03 board done, 40 issues, counts verified twice.
- next: Record the #12 answer the reviewer relays (issue and palace) without repeating the question to the owner; no new app work, no IL or refresh yet.
- DISPATCH HOLD LIFTED 01:57Z (reviewer: takeover finished). Was (startup takeover): send
  nothing to that pane until the reviewer confirms its restart and exact snapshot
  restore. PM saving a checkpoint with pruning disabled (reported). Architect
  and poller protected by daemon stop (reported). No IL refresh.
- D03 notes (verified 01:56Z): PM finished 01:51Z; 40 issues #2-#41, all
  open, every one in a milestone: 1.0 20, 1.1 10, 1.2 5,
  Parked 5 (milestones API, sums to 40; paged issue list 40). No
  duplicate titles or duplicate leading IDs; helper list /tmp/d03-gen/final.tsv
  matches the repo numbers. 12 labels created (reported). Board:
  https://github.com/<owner>/<repo>/milestone/<n>. Not placed
  (reported): Q40/Q41 conflict filed as question #12; two items treated as
  built; three 1.1 items filed in Parked because the ledger says deferred by
  the owner (#30-#33). Later: A01 import check when the owner says done; D05 and the
  D04 explanation to the owner.
- poll_count: 9 (last pass 01:48Z, full poll; first regular task-loop run)
- A01: the owner answered "Architect Do" 01:17Z; guard refused the import
  (#100); the owner asked to run the three imports by hand from the example
  clone. Safe was empty at 01:19Z (verified).
- usage state: HOLD since 2026-01-01T01:49Z, kept on reviewer instruction.
  01:15Z fresh reading: 5h 8% (reset 04:40Z); week 64% (reset 01-03 09:00Z),
  68.0% of week passed, projection 94.1%. Stays HOLD: the Resume rule needs
  projection at or below 90% (reviewer correction 01:16Z; my PACE reading was
  wrong). Direct owner requests and processing answers proceed.
- builder band: OPEN since 2026-01-01T01:03Z (latest 01:19Z: L15 4.35 / 14
  cores = 0.31x, 600 GiB free, no swap; verified). HOLD 00:54Z-01:03Z, STOP
  NEW 00:39Z-00:54Z. No relay: the example-app PM has no queued builder.
- last_consumed_ts: 2026-01-01T00:53:56Z
- last_alert_ts: 2026-01-01T01:53:02Z
- PR #1 example-app (chore/pm-reliability-checkpoint, head 0000aaaa):
  OPEN, CLEAN, auto-merge off, diff CLAUDE.md +13 and rules-index +1
  (verified). v5 on remote head (verified). Fixtures finished. Superseded
  00:28Z: the owner chose QUESTION 17 C (hold the eight rules as input for a
  restructure; merge into no branch now). The baseline-branch question is
  closed by that answer. PR stays open and unmerged; do not close or merge
  without the owner.
- last full poll: 2026-01-01T01:19Z
- observation channel: tmux CLI

## Heartbeat

- monitoring: ACTIVE: task-loop cron 0000bbbb at 7,37 * * * * (session-only,
  expires after 7 days), created 01:13Z on build-host approval via the reviewer;
  replaced hourly heartbeat 0000cccc (deleted). Verified by CronList.
  Poller session tm-architect-poll running (verified 01:15Z).
- tmux socket: /private/tmp/tmux-501/default

## Watch set

The sessions you manage are listed in `CLAUDE.md`. Everything else is
observed only when it signals. Note here a session added or dropped this pass
until `CLAUDE.md` carries it.

None.

## Dashboard

| Project | tmux session / pane | Context | Current observation / next action |
| --- | --- | --- | --- |

## Pending questions

Questions now live on the dashboard (http://127.0.0.1:7890/, store
local/owner-queue; private HTTPS at the tailnet address in local/architect.json).
Server now pid 4242, detached (parent pid 1, no terminal); local and private
HTTPS both return 200 (verified 01:28Z). History: first started by the owner by
hand in a foreground Terminal (pid 4243, 2026-01-01T00:06Z). The Architect
writes the store through the server's HTTP API (POST /api/batch, Origin
header) because the guard still refuses scripts/dashboard-db.py (#101);
a guard fix is pending (QUESTION 11 A).

- A01 (answered 01:17Z, "Architect Do"; dashboard status answered,
  verified 01:28Z): the Architect's import was refused by the guard
  (#100); the owner was given the three imports to run in Terminal from the
  example clone. Safe still empty at 01:19Z (verified). Done only when the
  key names show in the example-app scope.
- Closed 00:26Z: D01 (other three projects low priority, no order), D02
  (leave stale files), A02 (Keychain entry exists, verified by exit status).
- D03 (answered 00:57Z, "Create the board now from the inferred plan";
  dashboard status answered, verified 01:28Z): milestones and labels
  created; one issue per Q-number deferred under usage HOLD.
- D04 answered 00:25Z in chat: example-app branch workflow rules adopted.
  Owed to the owner: plain-English explanation of the six rules. Not on dashboard
  (guard refusal, #100).
- D05 (open, chat only): copy the recovery-plan note into 1.0?
  Recommended: show it to the owner first.
- 00:32Z: the reviewer relays from now on; every direct chat message is
  the owner unless stated otherwise.
- QUESTION 17 CLOSED 00:28Z, the owner chose C: hold the eight rules as input for
  a restructure session's new CLAUDE.md; PR #1 stays open, unmerged. Owed:
  when the new CLAUDE.md is ready, check it against the eight rules and
  explain the gaps to the owner before anything goes in. Need from the owner later:
  how to reach the restructure session.
- QUESTION 18 moot (the owner is in chat; D05 asked in chat). Retired 00:32Z.
- QUESTION 16: text not recorded in local/ (finding). Probably the refresh
  question answered "Yes" (snapshot summary). Confirm via the reviewer.
- With the reviewer: Review provider choice.
- Review provider detail: one dispatch provider fails. (PR #1 baseline branch
  no longer open: closed by the owner's QUESTION 17 C.)
- QUESTION 14 retired 2026-01-01 (unadopt filing routed by the reviewer to the
  build-host Architect, message 0000dddd0000).
- Build check finished (reported): 1,000 passed / 4 failed; one asset step
  refused by guard; 3 tests need a missing golden data folder; the
  buildcheck-0000eeee worktree (1.2 GB) kept pending a decision.
- Intent Ledger not initialised: datg.py refused by the guard (#101).

## PM reliability work (via the reviewer)

- Brief v5: local/briefs/2026-01-01-pm-reliability-relay.md (block md5
  0000ffff). Plan, conflict read, critic, fixtures RESULTS.md in local/.
- On hold (owner, QUESTION 17 C): no merge now. If merged later: PM quotes the block heading at a checkpoint; Architect reviews
  the PM's next real helper brief before dispatch; one real pm-evidence
  palace note. PM answers pending: canonical persona copy; restructure kit
  and owner scenarios paths (unknown so far).
- Build-host P1 gaps #200-#204; fixes queued after #198/#199.

## Clone and registry (via the reviewer)

- Layout canonical; registry repaired 2026-01-01 (four projects); setup rule
  in local/instance.md. Legacy git metadata and 25 ledger entries left intact
  until a supported unadopt exists.

## Context-refresh queue

- tm-architect (self): 257,008 tokens at 01:53Z, over the 250k bar (poller
  alert). Deferred: the reviewer said no refresh until the reviewed bundle and the
  startup takeover finish. Refresh at the first safe boundary the reviewer allows,
  after the D03 owner-audit result is recorded.

## Improvement candidates

None.
