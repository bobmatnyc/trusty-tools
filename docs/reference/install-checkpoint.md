# Install Checkpoint Runbook

Use this runbook to install a new trusty-mpm build (`tm` and the daemon) on a
machine that has live managed sessions. It checkpoints every session first,
installs from a pinned git rev, restarts the daemon under launchd, relaunches
every session that needs a tmux server pairing, and says how to roll back.

It covers an unreleased build from a rev. A published release installs from the
registry instead: [release-workflow.md](release-workflow.md#release-steps),
step 9. Background on the install rules:
[worktree-discipline.md](worktree-discipline.md#installing-a-freshly-built-binary),
[ADR-0043](../adr/0043-cargo-bin-policy.md).

Origin: [#9032](https://github.com/bobmatnyc/trusty-tools/issues/9032), from the
2026-10-01 P1 install run, plus the #9004 relaunch step (Architect ruling
2026-10-02 16:14Z, fail closed).

## Three facts that shape the order

1. **The daemon restart is not free.** On SIGTERM the daemon's graceful
   shutdown stops every live managed session (`SessionManager::shutdown` in
   `crates/trusty-mpm/src/session_manager/restart_ops.rs`). The Architect
   (`kind` `supervisor`) session is skipped. Checkpoint every session before
   you stop the daemon.
2. **#9004 pairs a record with its tmux server.** From the build that contains
   #9004 (PR #9094), a record stores the tmux server it was created on
   (`tmux_server`, `<pid>:<start_time>`). Stop, delete and decommission act
   only when that value matches the live server.
3. **A session live at install time has no pairing, and tm will not stop,
   delete or decommission it** until it is relaunched. A record with no
   `tmux_server` fails closed. Resuming it while its tmux session is still live
   does not record the server; it is recorded when a resume creates a new tmux
   session (the #9004 changelog fragment in PR #9094).

## 0. Set up

Work from outside the checkout so no `.envrc`, `[patch]` table or relative path
applies. Keep a notes file for the run:

```bash
cd "$HOME"
CKPT="$HOME/install-checkpoint-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$CKPT"
```

## 1. Pick and verify the rev

Install a full 40-character SHA from `main`, never a branch name.

```bash
gh pr view 9094 --repo bobmatnyc/trusty-tools --json state,mergeCommit
git -C <main-checkout> fetch origin main
git -C <main-checkout> merge-base --is-ancestor <merge-commit> <rev> && echo contains-9004
```

- `<main-checkout>` is a checkout of this repo. Use `git -C`; never `cd` into
  it. `fetch` only, never `pull`.
- PR #9094 must read `MERGED`. If it is still `OPEN`, the build has no #9004
  and steps 6 and 7 do not apply yet.
- `<merge-commit>` is the commit on `main` that PR #9094 produced. Use `<rev>`
  only if the `echo` prints.

## 2. Record the old state

```bash
tm --version                                    | tee    "$CKPT/old-tm-version.txt"
cargo install --list | grep -A2 '^trusty-mpm'   | tee    "$CKPT/old-cargo-install.txt"
curl -s http://127.0.0.1:7880/health | jq '{version,build_id,pid,supervised}' \
                                                | tee    "$CKPT/old-health.json"
tm status 2>&1 | head -3                        | tee    "$CKPT/old-status.txt"
launchctl print gui/$(id -u)/com.trusty.mpm | grep -E 'state|pid' \
                                                | tee    "$CKPT/old-launchd.txt"
tm sessions ls --json --no-prune | jq -r 'if type=="array" then . else .sessions end
  | .[] | [.id, .name, .state, .kind] | @tsv'   | tee    "$CKPT/sessions-before.tsv"
cp ~/.trusty-mpm/session-manager/sessions.json "$CKPT/sessions-before.json"
```

- The old rev is in the `cargo install --list` line, for example
  `trusty-mpm v1.7.10 (https://github.com/bobmatnyc/trusty-tools?rev=<sha>#<sha8>)`.
  A registry install shows no URL. This line is your rollback target.
- `/health` carries `version`, `build_id` and `pid`. Port 7880 is the default;
  `TRUSTY_MPM_URL` overrides it. `tm status` prints `daemon: reachable (pid N,
  version V)`.
- The `cp` copies a data file. The ban on `cp` applies to binaries.

## 3. Pre-install checkpoint: pause every live session

A checkpoint is each session's own `/tm-session-pause`. Take one for every
session in `sessions-before.tsv` whose state is `active`, the Architect session
included. The skill writes
`<project>/.trusty-mpm/sessions/<session-id>/session-YYYYMMDD-HHMMSS.md`,
appends a `pause` line to `sessions-log.jsonl`, and publishes a session ref to
`origin` (skill text:
`~/.trusty-tools/trusty-mpm/claude-config/skills/tm-session-pause/SKILL.md`).

1. In each session, run `/tm-session-pause <one line on current work>`. To ask
   a session from outside, `tm sessions send <ID> <TEXT>` injects text into its
   pane. Its `--help` does not say whether it submits the line, so check the
   result in step 2 below.
2. Verify per project directory. A session is checkpointed only when a new
   `pause` line names it:

   ```bash
   tail -n 5 <project>/.trusty-mpm/sessions/sessions-log.jsonl
   ```

   A line looks like
   `{"session_id":"<id>","event":"pause","snapshot":"<id>/session-<ts>.md","timestamp":"<rfc3339>"}`.
   The timestamp must be later than the start of this run. The snapshot file
   must exist.
3. In each pause result, read `ref_published` and `skipped_dirty_worktrees`.
   `ref_published: false` with a `ref_error` means the durable copy did not
   reach `origin`. A dirty worktree still holds unsaved work. Report both to the
   operator and stop for a decision.

Do not continue until every active session has a verified snapshot. Resume
later with `/tm-session-resume` ("Resume" in step 7).

## 4. Install

```bash
cd "$HOME"
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  env -u CARGO_TARGET_DIR \
  cargo install --git https://github.com/bobmatnyc/trusty-tools \
    --rev <rev> trusty-mpm --locked 2>&1 | tee "$CKPT/install.log"
echo "EXIT=${PIPESTATUS[0]}"
```

Rules, each checked against the repo:

- `--git` plus `--rev <full-sha>` plus `--locked`. Add `--force` only if cargo
  says the package is already installed.
- Never `cp` a binary into `~/.cargo/bin`: the next exec is SIGKILLed with an
  invalid signature
  ([release-workflow.md](release-workflow.md#macos-code-signing-critical-alert)).
- Never `cargo install --path`, and never from a worktree
  ([ADR-0043](../adr/0043-cargo-bin-policy.md)).
- `CARGO_NET_GIT_FETCH_WITH_CLI=true` makes cargo fetch with the `git` CLI, so
  your git credential helper applies.
- `env -u CARGO_TARGET_DIR` drops the shared target dir. `cargo install` then
  builds in its own temporary directory. An install target dir must sit
  outside `~/.trusty-tools/cargo-target`: `tm build-lease` treats everything
  under it as shared
  (`crates/trusty-mpm/src/core/build_lease/target_dir.rs`). `cargo install` is
  on the hook's heavy-build list, so an agent run is rewritten to `tm
  build-lease -- cargo install …`. Expect a cold build of the whole graph
  ([agent-cost-controls.md](agent-cost-controls.md#2-the-shared-cargo_target_dir-comes-from-the-repo-local-envrc)).
- Read the exit code, not the log tail. `PIPESTATUS[0]` is `cargo`'s status.

Then confirm what landed, and sign if you use Developer ID signing:

```bash
cargo install --list | grep -A2 '^trusty-mpm'   # must show ?rev=<rev>
command -v tm                                    # must be ~/.cargo/bin/tm
tm --version
tctl sign trusty-mpm                             # only with a Developer ID cert
```

`tm doctor` reports `binary_provenance` as `Warn` for a git install ("pinned to
a commit"). That is expected here
(`crates/trusty-mpm/src/core/binary_provenance.rs`).

## 5. Restart the daemon

launchd owns the daemon (label `com.trusty.mpm`), so `tm restart` refuses.
Use the graceful pair from
[release-workflow.md](release-workflow.md#connection-safe-daemon-restart-issue-534).
Do not use `kickstart -k`: it SIGKILLs the daemon and skips the session drain.

```bash
launchctl print gui/$(id -u)/com.trusty.mpm | grep -E 'state|pid'   # launchd's pid
lsof -nP -iTCP:7880 -sTCP:LISTEN                                    # who holds the port
```

If the two pids differ, an orphan holds the port. Stop here and apply
[the orphan-listener rule](release-workflow.md#one-time-fda-grant-after-first-signed-install)
before you continue.

```bash
launchctl bootout   gui/$(id -u) ~/Library/LaunchAgents/com.trusty.mpm.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.mpm.plist
```

## 6. Health checks and new pid and version

A 200 from `/health` alone proves nothing: a client racing the restart can
spawn an orphan daemon
([release-workflow.md](release-workflow.md#one-time-fda-grant-after-first-signed-install)).
Check launchd's own view too.

```bash
curl -s http://127.0.0.1:7880/health | jq '{status,version,build_id,pid,supervised,degraded}' \
  | tee "$CKPT/new-health.json"
launchctl print gui/$(id -u)/com.trusty.mpm | grep -E 'state|pid' | tee "$CKPT/new-launchd.txt"
tm status 2>&1 | head -3
tm doctor
```

Pass when all hold:

- `status` is `ok`, `degraded` is empty.
- `pid` in `new-health.json` differs from `old-health.json`, and equals the
  `pid` in `new-launchd.txt`. Same pid means the restart did not happen.
- `build_id` differs from `old-health.json`. `version` may stay the same when
  only the rev changed.
- `tm doctor`: `daemon_orphan` is `OK`. `.supervised: true` alone is not proof
  (a reparented orphan reads true); the pid comparison is.

## 7. Relaunch every session that has no pairing

### 7.1 Inventory

```bash
jq -r '.sessions | to_entries[] | .value | select(.state=="active" or .state=="stopped")
  | [.id[0:8], .tmux_name, .state, .kind, (.tmux_server // "NONE")] | @tsv' \
  ~/.trusty-mpm/session-manager/sessions.json | tee "$CKPT/pairing-after-restart.tsv"
```

`sessions.json` holds `.sessions` as an object keyed by session id. A row with
`NONE` has no pairing. Expect two groups:

- `active` with `NONE`: still live, no server identity. This includes the
  Architect session. Relaunch it (7.2).
- `stopped`: the shutdown stopped it. Resume it (7.3).

### 7.2 Relaunch a live, unpaired session

The script is session-relaunch.sh, under scripts/ in the **supervisor** project
(`bobmatnyc/supervisor`), not in this repo. Run it from that checkout, one
session at a time:

```bash
<supervisor>/scripts/session-relaunch.sh [--stop-tasks] <session> [log]
```

- `<session>` is the tmux session name (the `name` column of `tm sessions ls`,
  for example `tm-dogfood`).
- Precondition: step 3 is verified for this session, and its input box is empty
  or shows a suggestion.
- It sends `/exit`, waits for the shell, runs `tm`, then `/rename`,
  `/remote-control` and `/tm-session-resume`. It logs one line per step to
  `<supervisor>/inbox/relaunch.log` unless you pass `[log]`.
- It stops, with exit 1, and logs `SKIP input=…`, `STOPPED: background work
  running`, `EXIT-FAILED` or `LAUNCH-TIMEOUT`. For `STOPPED`, rerun with
  `--stop-tasks` only if losing the background work is acceptable.
- It keeps the tmux session alive. Per fact 3, a resume into a still-live tmux
  session may not record the server. Run 7.4 after every relaunch; do not
  assume.

### 7.3 Resume a stopped session

```bash
tm sessions resume <ID_OR_NAME>
```

The resume creates a new tmux session, which records the pairing. Then
`/tm-session-resume` inside it to reload the checkpoint.

### 7.4 Verify the pairing for each relaunched session

```bash
NAME=<tmux-session-name>
jq -r --arg n "$NAME" '.sessions | to_entries[] | .value | select(.tmux_name==$n)
  | [.id[0:8], .state, (.tmux_server // "NONE")] | @tsv' ~/.trusty-mpm/session-manager/sessions.json
env -u TMUX tmux display-message -p -t "=$NAME:" '#{pid}:#{start_time}'
```

The two `<pid>:<start_time>` values must be equal. `NONE`, or a mismatch, means
the session is still unpaired.

If a session is still `NONE` after 7.2, tm cannot end its tmux session (fail
closed). The operator ends it by exact target, after the checkpoint is
verified, then resumes:

```bash
tmux kill-session -t "=$NAME"
tm sessions resume $NAME
```

This is an operator decision. It terminates that session's agents. `tm hook
--pm-guard` may deny a `kill-session` from an agent session. Re-run 7.4.

## 8. Post-install live checks

Run against the installed binary, not a debug build.

1. Version and source: `tm --version`, and `cargo install --list` shows the new
   `?rev=`.
2. Sessions are all back. Compare ids, names, states and kinds with
   `sessions-before.tsv`; never the row index, which renumbers across a
   restart:

   ```bash
   tm sessions ls --json --no-prune | jq -r 'if type=="array" then . else .sessions end
     | .[] | [.id, .name, .state, .kind] | @tsv' | sort > "$CKPT/sessions-after.tsv"
   sort "$CKPT/sessions-before.tsv" | diff - "$CKPT/sessions-after.tsv"
   ```

   Every difference needs an explanation (a session you stopped, a pruned
   record).
3. Every `active` row in `pairing-after-restart.tsv` now has a `tmux_server`
   (7.4).
4. Kill guard accepts a paired session. Create a throwaway session, check its
   pairing, stop it, confirm the tmux session is gone. `<ABS_PATH>` must be an
   absolute path to an existing directory; `.` is refused.

   ```bash
   tm sessions new <ABS_PATH> --task "install check" --no-inject --name-hint install-check
   tm sessions ls --no-prune | grep install-check      # note the name
   # 7.4 for that name: a non-NONE tmux_server
   tm sessions stop <name>
   tmux has-session -t "=<name>"                       # must fail: session is gone
   ```

   Do not `decommission` it: that removes the workspace from disk.
5. The Architect session answers and is paired.
6. `tm doctor` has no new `Fail`.

## 9. Rollback

Roll back when step 6 fails, or when a step 8 check shows a regression you can
attribute to the new build. The target is the old line in
`old-cargo-install.txt`.

1. Checkpoint every active session again (step 3). The shutdown in the next
   step stops them.
2. Reinstall the old build:

   ```bash
   cd "$HOME"
   # old build was a git rev:
   CARGO_NET_GIT_FETCH_WITH_CLI=true env -u CARGO_TARGET_DIR \
     cargo install --git https://github.com/bobmatnyc/trusty-tools --rev <old-rev> trusty-mpm --locked
   # old build was a registry release:
   env -u CARGO_TARGET_DIR cargo install trusty-mpm --version <old-version> --locked
   ```

   Add `--force` only if cargo refuses with an already-installed message.
3. Restart the daemon (step 5) and run the step 6 checks. `cargo install
   --list` and `version` must match `old-cargo-install.txt` and
   `old-health.json`; `pid` and `build_id` differ, because both are new.
4. Resume stopped sessions (7.3).
5. `sessions.json` needs no restore. The old build ignores the `tmux_server`
   field (the record types set no `deny_unknown_fields`). The copy in
   `sessions-before.json` is for a corrupt store only; stop the daemon before
   you put it back.

A rollback returns the pane-id-only ownership check from #8935. The
server-restart collision that #9004 closes can recur until you reinstall the new
build.

## Gaps and open items

- `session-relaunch.sh` lives in the supervisor project, not in this repo. This
  runbook cannot link it as a path, and a change to it is not covered by this
  repo's gates.
- Whether the script's in-place relaunch records the server is not established
  by the code in this repo. The #9004 changelog says a resume into a live tmux
  session does not. Step 7.4 is the proof, and its fallback is the operator
  kill.
- `tm sessions send` does not document whether it submits the text.
- [#9101](https://github.com/bobmatnyc/trusty-tools/issues/9101): daemon
  shutdown kills by name. Step 3 stays mandatory until it closes.
