#!/usr/bin/env python3
"""datg.py: per-PM goals/decisions/actions/tasks store and hook logic (stdlib only).

Why: pilot of the PM D/A/T/G design (local/briefs/2026-10-09-pm-datg-prototype-design.md,
the owner D93, 2026-10-09 13:28Z): keep a PM on ranked goals. Only the PM writes, one call per turn.
What: <project>/.trusty-mpm/datg/state.json (snapshot, replaced atomically: temp file +
os.replace, under flock on .lock) and log.jsonl (append-only, one line per change).
Verbs: init, add, update, close, batch, session set, prune, rotate, stamp (write); show,
check-dispatch, digest, fresh, archive-show, hook (read-only apart from log lines).
A write refuses (exit 2, files untouched) when state.json is malformed or a field is invalid.
`hook` serves SessionStart (rules + digest), Stop (stamp) and PreToolUse(Agent|Task); every
error path prints nothing, exit 0.
Intent Ledger (docs/specs/SPEC-IL-01.md), interface choices where the spec is open:
- Layout: legacy <project>/.trusty-mpm/datg/ keeps working. `--session NAME` (init forces it)
  selects <project>/.trusty-mpm/datg/NAME/; $DATG_SESSION (set by datg-hook.sh) or the tmux
  session of $TMUX_PANE selects it only when that directory already holds a state.json.
- session record: `session set [--task T] [--next N] [--wait who:what ..] [--owe check=expect@ref ..]
  [--agent id:goal:what ..] [--cron id=schedule=prompt ..] [--boundary] [--ctx-use A:S:CTX:keep] [--ref KEY=VALUE ..]`.
  `--ref` pointers (the Google Doc, its source md, the relay marker) live in session.refs [{key, value}]: KEY
  ^[a-z0-9][a-z0-9_.-]{0,31}$, VALUE one line <= 200 chars, <= 12 refs; an existing KEY is replaced, `none` clears.
  The digest prints them first under POINTERS and cuts them after items. Omitted fields are kept; a repeatable flag replaces its list; value `none` states an empty list
  (`--owe none` sets owed_none). Batch entry: {VERB_KEY:"session","task":..,"waiting_on":[{who,what}],
  "owed":[{check,expect,ref}],"agents":[..],"crons":[..],"boundary":true,"ctx_use":{..}}.
  Every commit stamps session.written_at (ISO minute) and written_epoch (int seconds).
- close / `update --status done` / prune move D, A, T items to archive.jsonl in the same commit
  (archive first, line {archived_at, closed_at, kind, item, cv}; cv = state v of the commit, a
  line with cv > state v is an orphan of a failed commit and is dropped under the next lock).
  `prune [--dry-run] [--answered-hours 24] [--require-outcome]` archives every done item/task and
  every answered item older than 24 h (spec says "with outcome"; the pilot's 36 answered items
  have none, so outcome is required only with --require-outcome). `archive-show ID` prints one line.
- Size bar: state.json > 32 KB logs `size_warning` on crossing; a write that leaves it > 64 KB and
  larger than before is refused (exit 2, largest items named) after one automatic prune attempt.
- `rotate`: log lines older than 7 days go to log-<YYYYMMDD>.jsonl (rotation day). Both log shapes
  (t/ev and ts/kind) are read.
- `digest [--max-tokens 3000] [--no-log]`: bounded resume digest on stdout (tokens = chars/4);
  appends a `digest` log line unless --no-log.
- `fresh [--since ISO|EPOCH] [--max-age MIN]`: stdout one line; exit 0 fresh, 1 stale, 2 unreadable.
  Fresh = written_at not before --since (default: the last unanswered change recorded by `stamp`,
  none = fresh) and, with --max-age, not older than MIN minutes.
- `stamp` (Stop hook; hook JSON on stdin with transcript_path): if the finished turn used a
  state-changing tool (Edit/Write/Agent/Task/SendMessage/Cron*, or Bash git/gh mutations) and no
  datg.py write followed, append {ts, session, kind: stale_il, n} to log.jsonl (n = consecutive
  stale turns) and record stamp.json changed_epoch for `fresh`.
Escalation: `update --esc architect` (or the same field in a batch entry) also queues a `decision` through the D10
intake (scripts/pm-notify.sh, override DATG_NOTIFY_CMD); a failure is logged, never fatal.
A warn-mode hook warning is appended to log.jsonl as kind hook_warning (ts, session, text).
Dispatch policy: warn until config.block_after (default 2026-10-12T13:30Z), then deny a
dispatch whose prompt lacks "Goal: <code>-G<hex2>" naming an active goal; config.mode
("warn"|"block") overrides the date. No network, no secrets (locations only in items).
Layout (#10161, 500-SLOC cap): this file is the CLI, the hook entry and a facade. The logic
lives in datg_store.py (state files, commit, lock; `_SESSION`), datg_ops.py (write ops),
datg_log.py (prune, archive, rotate, notices) and datg_read.py (digest, freshness, stamp);
every name they define stays importable as datg.<name>. Ship all five side by side.
Test: scripts/test_datg.py
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:  # #10161: -P / PYTHONSAFEPATH drop the script dir; the datg_* modules live beside this file
    sys.path.insert(0, HERE)
import datg_store  # noqa: E402  (#10161: main() sets datg_store._SESSION, which per_pm_session reads)
from datg_store import VERB_KEY  # noqa: E402  (#10571: a batch entry's verb key)
from datg_ready import op_ready  # noqa: E402  (#10579: datg.py ready)
from datg_store import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    CODE_RE, DEFAULT_BLOCK_AFTER, DatgError, EVENTS, ID_RE, Locked, PREFIXES, RULES, SESSION_MAX, SESSION_RE,
    SIZE_REFUSE, SIZE_WARN, STATUS, TITLE_MAX, VAGUE, WHO, _atomic_write, _dump, _jl, _largest, _size,
    _tmux_session, answered_times, commit, datg_dir, load_state, log_rec, now_iso, parse_iso, parse_ts, paths,
    per_pm_session, prune_state, read_log, reconcile_archive,
)
from datg_ops import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    REFS_MAX, REF_KEY, _all_ids, _bad, _check_goal, _check_opts, _find, _int, _lim, _merge_refs, _next_id,
    _opts, _rows, _title, apply_add, apply_op, apply_session, apply_update, collect_closed, op_init,
    write_ops_ev,
)
from datg_log import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    append_log, archive_lookup, notify_escalations, op_prune, op_rotate, pm_session, write_ops,
)
from datg_read import (  # noqa: E402,F401  (#10161: re-exported for callers and tests)
    MUTATING_BASH, MUTATING_TOOLS, _digest_sections, _read_json, _short, _stamp_path, _to_epoch, build_digest,
    check_dispatch, dispatch_mode, log_hook_warning, op_fresh, stamp_turn, summary, turn_tools,
)


# #10109: Claude Code caps a hook's additionalContext at 10,000 characters; keep margin under it.
HOOK_CONTEXT_MAX = 9500


def session_start_context(st, project):
    """SessionStart context for every source (startup, resume, clear, compact), within HOOK_CONTEXT_MAX.

    Why: #10109, a session cleared or compacted must resume from its ledger with no typed prompt.
    What: rules + summary + the digest, bounded to the hook limit; a cut digest ends with the
    command that prints it in full. A digest failure is reported on stderr; rules still go out.
    Test: test_clear_and_compact_carry_the_digest, test_large_digest_stays_under_the_hook_limit.
    """
    py = os.path.abspath(__file__)
    pm = per_pm_session(project)
    cli = py + (f" --session {pm}" if pm else "")
    s = summary(st)
    head = RULES.format(py=cli, code=st["code"]) + (" " + s if s else "")
    ptr = f"FULL DIGEST: python3 {cli} digest"
    room = HOOK_CONTEXT_MAX - len(head) - len(ptr) - 2
    try:
        dig = "\n" + build_digest(project, max(50, room // 4))
    except Exception as e:  # #10109: fail open, but visibly
        print(f"datg hook: SessionStart digest failed ({e})", file=sys.stderr)
        dig = ""
    ctx = head + dig
    if "\nTRUNCATED:" in dig or len(ctx) > HOOK_CONTEXT_MAX:
        ctx = ctx[:HOOK_CONTEXT_MAX - len(ptr) - 1] + "\n" + ptr
    return ctx


def hook_output(event, st, payload, project, now=None):
    """Return the hook JSON dict for the event, or None for no output."""
    if event == "SessionStart":
        return {"hookSpecificOutput": {"hookEventName": "SessionStart",
                                       "additionalContext": session_start_context(st, project)}}
    if event == "Stop":
        stamp_turn(project, payload)
        return None
    if event == "PreToolUse" and payload.get("tool_name") in ("Agent", "Task"):
        ti = payload.get("tool_input") or {}
        dec, msg = check_dispatch(st, ti.get("prompt") or "", now)
        if dec == "block":
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                           "permissionDecision": "deny",
                                           "permissionDecisionReason": msg}}
        if dec == "warn":
            log_hook_warning(project, msg)
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                           "additionalContext": msg + " (warn mode)"}}
    return None


def run_hook(stdin_text):
    """Fail open: any problem returns "" (no output). No ledger is silent; any other failure is one stderr line."""
    try:
        payload = json.loads(stdin_text or "{}")
        if not isinstance(payload, dict) or payload.get("agent_id"):
            return ""
        project = os.environ.get("CLAUDE_PROJECT_DIR") or payload.get("cwd") or os.getcwd()
        if not os.path.exists(paths(project)[1]):
            return ""  # #10109: a session without a ledger gets nothing and no error
        st = load_state(project)
        out = hook_output(payload.get("hook_event_name"), st, payload, project)
        return json.dumps(out, separators=(",", ":")) if out else ""
    except Exception as e:  # #10109: never block the session, but report the failure
        print(f"datg hook: failed open ({e})", file=sys.stderr)
        return ""


def cmd_show(project, brief):
    st = load_state(project)
    if brief:
        return summary(st) or "no open items"
    return json.dumps(st, indent=1, ensure_ascii=False)


def build_parser():
    p = argparse.ArgumentParser(prog="datg.py", description=__doc__.split("\n")[0])
    p.add_argument("--project", default=os.getcwd(), help="project root (default: cwd)")
    p.add_argument("--session", help="per-PM directory datg/<session>/ (shared project dirs)")
    sub = p.add_subparsers(dest="verb", required=True)
    s = sub.add_parser("init")
    s.add_argument("--code", required=True)
    s = sub.add_parser("add")
    s.add_argument("kind", choices=PREFIXES)
    s.add_argument("--title", required=True)
    s.add_argument("--goal")
    s.add_argument("--rank", type=int)
    s.add_argument("--finish")
    s.add_argument("--maps")
    s.add_argument("--opts", help="comma list, trailing * marks the recommendation")
    s.add_argument("--to")
    s.add_argument("--id")
    s = sub.add_parser("update")
    s.add_argument("id")
    for f in ("title", "status", "answer", "outcome", "esc", "to", "note"):
        s.add_argument(f"--{f}")
    s.add_argument("--rank", type=int)
    s = sub.add_parser("close")
    s.add_argument("id")
    s.add_argument("--outcome", required=True)
    s = sub.add_parser("batch", help="JSON list of ops on stdin: one write per turn")
    s = sub.add_parser("show")
    s.add_argument("--brief", action="store_true")
    s = sub.add_parser("check-dispatch")
    s.add_argument("--prompt", help="default: stdin")
    sub.add_parser("hook", help="Claude Code hook entry; JSON on stdin")
    s = sub.add_parser("session", help="set fields of the session record")
    s.add_argument("action", choices=("set",))
    s.add_argument("--task")
    s.add_argument("--next")
    s.add_argument("--hold", help="what the owner put on hold; 'none' clears it (#10579)")
    s.add_argument("--merge-hold", help="a no-merge hold, kept apart from --hold; 'none' clears it")
    s.add_argument("--wait", action="append", metavar="WHO:WHAT", help="repeatable; 'none' = empty list")
    s.add_argument("--owe", action="append", metavar="CHECK=EXPECT@REF", help="repeatable; 'none' = nothing owed")
    s.add_argument("--agent", action="append", metavar="ID:GOAL:WHAT", help="repeatable; 'none'")
    s.add_argument("--cron", action="append", metavar="ID=SCHEDULE=PROMPT", help="repeatable; 'none'")
    s.add_argument("--boundary", action="store_true")
    s.add_argument("--ctx-use", metavar="ACTIVE:STALE:CTX:KEEP")
    s.add_argument("--ref", action="append", metavar="KEY=VALUE", help="repeatable; same KEY replaces; KEY=none removes KEY; 'none' clears all")
    s = sub.add_parser("prune", help="archive done items/tasks and answered items older than 24 h")
    s.add_argument("--dry-run", action="store_true")
    s.add_argument("--answered-hours", type=float, default=24)
    s.add_argument("--require-outcome", action="store_true")
    sub.add_parser("rotate", help="move log lines older than 7 days to log-<YYYYMMDD>.jsonl")
    s = sub.add_parser("archive-show")
    s.add_argument("id")
    s = sub.add_parser("digest", help="bounded resume digest")
    s.add_argument("--max-tokens", type=int, default=3000)
    s.add_argument("--no-log", action="store_true")
    s = sub.add_parser("fresh", help="exit 0 fresh, 1 stale, 2 unreadable")
    s.add_argument("--since", help="ISO YYYY-MM-DDTHH:MMZ or epoch seconds")
    s.add_argument("--max-age", type=float, help="minutes")
    sub.add_parser("stamp", help="Stop hook step; hook JSON on stdin")
    s = sub.add_parser("ready", help="exit 0 ready to clear, 1 not ready (reasons printed), 2 missing or unreadable")
    s.add_argument("--json", action="store_true", help='print {"ready", "reasons", "written_at", "hold"}')
    s.add_argument("--since", help="ISO YYYY-MM-DDTHH:MMZ or epoch seconds (as for fresh)")
    s.add_argument("--max-age", type=float, help="minutes (as for fresh)")
    return p


def session_op(a):
    """Build the batch-style `session` entry from the CLI flags (strings parsed here, validated in apply_session)."""
    entry = {VERB_KEY: "session", "task": a.task, "next": a.next, "boundary": a.boundary}
    if a.hold is not None:
        entry["hold"] = "" if a.hold == "none" else a.hold   # #10579: HOLD is a session field
    if a.merge_hold is not None:
        entry["merge_hold"] = "" if a.merge_hold == "none" else a.merge_hold

    def lst(vals, parse):
        return [] if vals == ["none"] else [parse(v) for v in vals]

    def need(v, sep, n, fmt):
        parts = v.split(sep, n - 1)
        if len(parts) != n:
            raise DatgError(f"expected {fmt}, got {v!r}")
        return parts

    if a.wait is not None:
        entry["waiting_on"] = lst(a.wait, lambda v: dict(zip(("who", "what"), need(v, ":", 2, "WHO:WHAT"))))
    if a.owe is not None:
        def owe(v):
            check, expect_ref = need(v, "=", 2, "CHECK=EXPECT@REF")
            expect, _, ref = expect_ref.partition("@")
            return {"check": check, "expect": expect, "ref": ref}
        entry["owed"] = lst(a.owe, owe)
    if a.agent is not None:
        entry["agents"] = lst(a.agent, lambda v: dict(zip(("id", "goal", "what"), need(v, ":", 3, "ID:GOAL:WHAT"))))
    if a.cron is not None:
        entry["crons"] = lst(a.cron, lambda v: dict(zip(("id", "schedule", "prompt"), need(v, "=", 3, "ID=SCHEDULE=PROMPT"))))
    if a.ref is not None:
        def ref(v):
            key, val = need(v, "=", 2, "KEY=VALUE")
            return {"key": key, "value": None if val == "none" else val}   # KEY=none removes KEY
        entry["refs"] = lst(a.ref, ref)
    if a.ctx_use:
        act, stale, ctx, keep = need(a.ctx_use, ":", 4, "ACTIVE:STALE:CTX:KEEP")
        try:
            entry["ctx_use"] = {"active": int(act), "stale": int(stale), "ctx": int(ctx), "keep": keep}
        except ValueError:
            raise DatgError("ctx-use: ACTIVE, STALE, CTX must be integers")
    return entry


def main(argv=None):
    a = build_parser().parse_args(argv)
    datg_store._SESSION = a.session  # #10161: _SESSION lives in datg_store, which reads it
    try:
        if a.verb == "hook":
            out = run_hook(sys.stdin.read())
            if out:
                print(out)
            return 0
        if a.verb == "ready":   # #10579: the readiness check self-clear's il_ok calls (#10599)
            code, reasons, info = op_ready(a.project, a.since, a.max_age)
            if a.json:
                print(json.dumps({"ready": code == 0, "reasons": reasons, **info}))
            else:
                print("\n".join(reasons) if reasons else "ready: task, next, saved work, owed and crons recorded; fresh")
            return code
        if a.verb == "fresh":
            code, msg = op_fresh(a.project, a.since, a.max_age)
            print(msg)
            return code
        if a.verb == "init":
            print(op_init(a.project, a.code))
        elif a.verb == "add":
            entry = {VERB_KEY: "add", "kind": a.kind, "title": a.title, "goal": a.goal, "rank": a.rank,
                  "finish": a.finish, "maps": a.maps, "opts": _opts(a.opts), "to": a.to, "id": a.id}
            print(write_ops(a.project, [entry]))
        elif a.verb == "update":
            entry = {k: getattr(a, k) for k in ("title", "status", "answer", "outcome", "esc", "to", "note", "rank")}
            entry.update({VERB_KEY: "update", "id": a.id})
            evs, st = write_ops_ev(a.project, [entry])
            print(", ".join(e["id"] for e in evs))
            notify_escalations(a.project, evs, st)
        elif a.verb == "close":
            print(write_ops(a.project, [{VERB_KEY: "close", "id": a.id, "outcome": a.outcome}]))
        elif a.verb == "batch":
            try:
                ops = json.loads(sys.stdin.read())
            except ValueError as e:
                raise DatgError(f"batch input is not JSON ({e})")
            if not isinstance(ops, list):
                raise DatgError("batch input must be a JSON list")
            evs, st = write_ops_ev(a.project, ops)
            print(", ".join(e["id"] for e in evs))
            notify_escalations(a.project, evs, st)
        elif a.verb == "session":
            print(write_ops(a.project, [session_op(a)]))
        elif a.verb == "prune":
            print(op_prune(a.project, a.dry_run, a.answered_hours, a.require_outcome))
        elif a.verb == "rotate":
            print(op_rotate(a.project))
        elif a.verb == "archive-show":
            print(archive_lookup(a.project, a.id))
        elif a.verb == "digest":
            print(build_digest(a.project, a.max_tokens))
            if not a.no_log:
                try:
                    append_log(a.project, {"ts": now_iso(), "session": os.environ.get("DATG_SESSION", ""), "kind": "digest"})
                except OSError as e:
                    print(f"datg: log write failed ({e})", file=sys.stderr)
        elif a.verb == "stamp":
            try:
                payload = json.loads(sys.stdin.read() or "{}")
            except ValueError:
                payload = {}
            stamp_turn(a.project, payload if isinstance(payload, dict) else {})
        elif a.verb == "show":
            print(cmd_show(a.project, a.brief))
        elif a.verb == "check-dispatch":
            prompt = a.prompt if a.prompt is not None else sys.stdin.read()
            dec, msg = check_dispatch(load_state(a.project), prompt)
            print(dec + (": " + msg if msg else ""))
            return 2 if dec == "block" else 0
    except DatgError as e:
        print(f"datg: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
