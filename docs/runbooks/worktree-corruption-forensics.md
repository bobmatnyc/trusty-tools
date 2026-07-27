# Runbook — worktree corruption: forensic capture and triage

**Issue:** #3764 (item 3) · **Parent symptom:** #3715 · **Related:** #1845 (F3
canonicalize fallback), #1744 (cwd collision), #3721 (recreation guard)

**Read this the moment an alarm below fires. Capture first, diagnose second.**
In the last incident every pre-incident transcript was destroyed before anyone
looked: `write_scrollback` overwrites `.trusty-mpm/scrollback.txt` on the next
stop, and the shell history files held nothing. The forensic window closes in
minutes, not hours.

---

## 1. Which alarms bring you here

All four are ERROR-level, which means they are persisted to
`<data_dir>/trusty-mpm/errors.jsonl` by the daemon's bug-capture layer and are
queryable via the `list_recent_errors` MCP tool and `tm doctor`. WARN-level
events are **not** captured anywhere queryable — that asymmetry is why each of
these was deliberately escalated.

| Alarm text (grep for this) | Source | Means |
|---|---|---|
| `WORKTREE DESTROYED` | `session_manager::worktree_integrity` | An **Active** session's own worktree is no longer a git work tree. Its uncommitted work is already gone. |
| `SESSION REGISTRY CORRUPTION` | `daemon::cwd_collision` | ≥2 Active records resolve to one worktree path. The observed **precursor** state. |
| `WORKTREE CORRUPTION GUARD: refusing to decommission` | `session_manager::decommission` | A teardown was refused because the record pointed at a live peer's worktree. Nothing was deleted. |
| `active session path has failed to canonicalize for N consecutive` | `session_manager::prune` (#1845 F3 streak) | Sustained canonicalize failure on a live path — fired for ~8 h unnoticed before #3715. |

---

## 2. Capture — do this FIRST, before any diagnosis

Run all of it. Do not stop the session, do not resume it, do not run `tm` on
it, and above all do not let it reach a stop path — a stop triggers
`write_scrollback`, which overwrites the only surviving pane transcript.

```bash
# 0) Pick a capture dir OUTSIDE any managed worktree.
INC=~/worktree-incident-$(date +%Y%m%d-%H%M%S)
mkdir -p "$INC"

# 1) The session uuid / worktree leaf from the alarm.
UUID=<session-uuid-from-the-alarm>
WT=<workspace_path-from-the-alarm>

# 2) macOS unified log — the ONLY place out-of-band `rm -rf` / `git worktree
#    remove` from an interactive shell leaves any trace. daemon.log will be
#    silent on a manual deletion; that is the #3715 finding, not an omission.
log show --last 1h --predicate "eventMessage contains \"$UUID\"" > "$INC/unified-log-uuid.txt"
log show --last 1h --predicate 'eventMessage contains "worktree"' > "$INC/unified-log-worktree.txt"

# 3) EVERY pane's full scrollback. Not just the suspect pane — the deleter is
#    typically a DIFFERENT pane than the victim, which is the whole point of
#    the cross-session guard.
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_id} #{pane_current_path}' \
  > "$INC/panes.txt"
while read -r _ pane _; do
  tmux capture-pane -p -S - -t "$pane" > "$INC/pane${pane//%/}.txt" 2>/dev/null
done < <(awk '{print $1, $2, $3}' "$INC/panes.txt")

# 4) Daemon state, before anything mutates it.
cp ~/.trusty-mpm/daemon.log "$INC/" 2>/dev/null
cp ~/.trusty-mpm/logs/trusty-mpm.log* "$INC/" 2>/dev/null
cp "$(trusty-common-data-dir 2>/dev/null || echo ~/.local/share)/trusty-mpm/errors.jsonl" "$INC/" 2>/dev/null
tm ls > "$INC/tm-ls.txt" 2>&1

# 5) Filesystem evidence. ctime CANNOT be back-dated — it is the strongest
#    timestamp available and is what dated the third incident to the minute.
stat "$WT" > "$INC/stat-worktree.txt" 2>&1
ls -la@ "$WT" >> "$INC/stat-worktree.txt" 2>&1

# 6) Git-side evidence, from the BASE repo (not from inside the worktree —
#    from inside a stripped worktree git silently answers from the parent).
BASE=$(dirname "$(dirname "$WT")")
git -C "$BASE" worktree list > "$INC/worktree-list.txt" 2>&1
ls -la "$BASE/worktrees/$(basename "$WT")" >> "$INC/worktree-list.txt" 2>&1
git -C "$BASE" reflog --date=iso > "$INC/base-reflog.txt" 2>&1
```

---

## 3. Confirm the diagnosis

**Never trust `git status` or `git log` from inside the suspect directory.**
That is the fail-open at the heart of #3764 item 4: with the `.git` pointer
gone, discovery walks *up* to the enclosing repo, so `git log` prints plausible
commits from the parent's stale `main` while `git status` fatals — and a fatal
renders as `Status: (clean)` in the harness.

The one correct probe (matching `session_manager::worktree_integrity::check`):

```bash
git -C "$WT" rev-parse --show-toplevel
```

* prints `$WT` → the worktree is intact; look elsewhere.
* prints an **ancestor** directory → destroyed (parent clone is non-bare).
* `fatal: this operation must be run in a work tree` → destroyed (parent clone
  is bare — this repo's `.base` layout).

`git rev-parse --is-inside-work-tree` is **not** a substitute: it returns
`true` for a fully destroyed worktree whenever the parent clone is non-bare.
Verified empirically; see the module doc for the matrix.

---

## 4. Triage

1. **Do not "repair" the directory.** Recreating the root masks the loss —
   exactly the #3715 behaviour the `write_scrollback` guard was added to stop.
2. **Check for a cwd collision** (`SESSION REGISTRY CORRUPTION` in the log, or
   duplicate `workspace_path`s in `tm ls`). If ≥2 Active records share the
   path, reconcile them (`tm sessions delete <stale-id>`) before touching the
   filesystem — a decommission of the wrong record is what destroys the tree.
3. **Recover work, if any survived**, from `$BASE`'s reflog and any
   `session/<leaf>` branch: `git -C "$BASE" log --all --oneline`.
4. **Stop the blinded session.** An Active session in a destroyed worktree is
   still running and still committing into nothing.
5. **File with the capture attached**, referencing #3715 and #3764.

---

## 5. Why this is a runbook and not an automated hook

Automating the capture on the F3 streak trip was evaluated for #3764 item 3 and
deliberately **not** wired:

* `log show --last 1h` and a full `tmux capture-pane -S -` across every pane are
  seconds-to-minutes of blocking I/O. The F3 streak trips **inside the orphan-GC
  sweep**, on the daemon's async executor, once per ~60 s tick — and it trips
  repeatedly by construction (that is what a *streak* is). Wiring capture there
  would stall the sweep on a timer, indefinitely, in exactly the degraded state
  where the sweep's other duties matter most.
* The capture must write outside every managed worktree, but the daemon has no
  incident-scoped location and would be inventing a retention policy, a disk
  budget, and a redaction pass (pane scrollback contains secrets — the existing
  `snapshot::REDACT_RE` covers the scrollback path only) to do it safely.
* The alarms are now ERROR-level and therefore land in `errors.jsonl` /
  `list_recent_errors` the moment they fire. Detection — the thing that was
  actually missing for three days — no longer depends on capture.

Revisit if a bounded, redacting, out-of-band capture helper lands; the item-3
requirement ("at minimum ship the runbook") is satisfied by this document.
