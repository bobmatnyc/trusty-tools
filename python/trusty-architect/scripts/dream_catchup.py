#!/usr/bin/env python3
"""dream_catchup.py: daily digest of what the owner said and decided since the last owner/ dreaming pass.

Why: the owner model (owner/) lapsed because nothing prompted the dreaming pass, so the owner asked for a daily
digest: a script digests new prompts, answers and rulings, a research agent updates owner/, the owner reviews and
commits. This script is the deterministic first half; the research agent is the second.
What: reads (a) Owner-authored turns in the Architect transcripts newer than the last timestamp in
owner/dream-log.md (human-origin string turns only; no hook output, system reminders, tool results,
task notifications, sidechains, "[poll " wakes, slash-command echoes or resume boilerplate), and (b) dashboard
items answered since then (the live store via dashboard-db.py list items, plus the store's pruned.jsonl),
classified took-rec / other-option / free-text against the options' `recommended` flag. Writes ONE file,
local/briefs/dream/<local date>-digest.md: prompt count, the prompts (300 chars, anything credential-like is
dropped), answered decisions, uptake % and the override list. trusty-memory is the agent's job, not read here.
due(state, now, root) is the poller's once-per-local-day clock (fleet-poll.py dream_alerts).
Usage: dream_catchup.py [--since ISO] [--out DIR] [--root DASHBOARD_ROOT] [--transcripts DIR] [--log FILE]
Test: tests/test_dream_catchup.py
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
if HERE not in sys.path:     # python3 -I drops the script directory
    sys.path.insert(0, HERE)
import architect_config  # noqa: E402
SKIP_PREFIXES = ("<", "[poll ", "Resume from the Architect handoff", "Caveat:")
CRED = re.compile(
    r"(sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}"
    r"|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}|Bearer\s+[A-Za-z0-9._~+/=-]{16,}|-----BEGIN [A-Z ]*PRIVATE KEY"
    r"|(?i:password|passwd|secret|token|api[_-]?key)\s*[:=]\s*\S{6,}|\b[A-Fa-f0-9]{40,}\b|\b[A-Za-z0-9+/_-]{48,}={0,2})")
DREAM_TIME = (6, 30)   # local hour, minute of the daily alert
LOG_TS = re.compile(r"^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d)([0-9x])Z")


def default_transcripts(environ=None):
    """The Architect's transcript directory: <claude_config_dir>/projects/<slug of this checkout>.
    #10571 S2: read from local/architect.json (CLAUDE_CONFIG_DIR overrides), never a home path in the script.
    The slug is Claude Code's: every character that is not an ASCII letter or digit becomes "-"."""
    cfg = architect_config.load(root=ROOT, environ=environ)
    return os.path.join(cfg.claude_config_dir, "projects", re.sub(r"[^A-Za-z0-9]", "-", ROOT))


def last_logged(log_path):
    """Latest timestamp at the head of a dream-log line, as aware UTC; None when absent. 'x' digits mean 0."""
    best = None
    try:
        lines = open(log_path, encoding="utf-8").read().splitlines()
    except OSError:
        return None
    for line in lines:
        m = LOG_TS.match(line)
        if m:
            minute = m.group(3) + ("0" if m.group(4) == "x" else m.group(4))
            ts = datetime.fromisoformat(f"{m.group(1)}T{m.group(2)}:{minute}:00+00:00")
            best = ts if best is None or ts > best else best
    return best


def parse_ts(text):
    try:
        return datetime.fromisoformat(str(text).replace("Z", "+00:00")).astimezone(timezone.utc)
    except ValueError:
        return None


def scrub(text, limit=300):
    """Prompt text cut to `limit`, or None when it looks like a credential (the whole prompt is dropped)."""
    return None if CRED.search(text) else (text if len(text) <= limit else text[:limit] + "...")


def owner_prompts(transcripts, since):
    """[(ts, text)] of the owner's turns newer than `since`, oldest first, plus the count dropped as credential-like."""
    out, dropped = [], 0
    for path in glob.glob(os.path.join(transcripts, "*.jsonl")):
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if '"type":"user"' not in line and '"type": "user"' not in line:
                    continue
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                content = (r.get("message") or {}).get("content")
                origin = r.get("origin") or {}
                if (r.get("type") != "user" or not isinstance(content, str) or r.get("isMeta")
                        or r.get("isSidechain") or origin.get("kind") != "human"):
                    continue
                text = content.strip()
                if not text or text.startswith(SKIP_PREFIXES):
                    continue
                ts = parse_ts(r.get("timestamp"))
                if ts is None or (since is not None and ts <= since):
                    continue
                shown = scrub(text)
                if shown is None:
                    dropped += 1
                else:
                    out.append((ts, shown))
    out.sort(key=lambda p: p[0])
    return out, dropped


def classify(data):
    """('rec' | 'other' | 'free', answer) for an answered item, None when it holds no answer."""
    answer = (data.get("answer") or "").strip()
    if not answer:
        return None
    for opt in data.get("options") or []:
        if isinstance(opt, dict) and (opt.get("label") or "").strip() == answer:
            return ("rec" if opt.get("recommended") else "other"), answer
    return "free", answer


def answered_items(root, since):
    """Answered items since `since` from the live store and pruned.jsonl, de-duplicated on (id, answered_at)."""
    docs = []
    proc = subprocess.run([sys.executable, os.path.join(HERE, "dashboard-db.py"), "--root", root, "list", "items"],
                          capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        raise RuntimeError(f"dashboard-db list items failed: {proc.stderr.strip()[:200]}")
    docs += [(d["id"], d["data"]) for d in json.loads(proc.stdout)]
    pruned = os.path.join(os.path.dirname(os.path.abspath(root)), "owner-queue-archive", "pruned.jsonl")
    if os.path.exists(pruned):
        with open(pruned, encoding="utf-8") as fh:
            for line in fh:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if r.get("collection") == "items" and isinstance(r.get("data"), dict):
                    docs.append((r.get("id"), r["data"]))
    seen, out = set(), []
    for item_id, data in docs:
        at = parse_ts(data.get("answered_at"))
        if at is None or (since is not None and at <= since) or (item_id, data.get("answered_at")) in seen:
            continue
        cls = classify(data)
        if cls is None:
            continue
        seen.add((item_id, data.get("answered_at")))
        out.append({"id": item_id, "at": at, "kind": cls[0], "answer": cls[1], "title": data.get("title", ""),
                    "rec": next((o.get("label") for o in data.get("options") or []
                                 if isinstance(o, dict) and o.get("recommended")), None)})
    out.sort(key=lambda a: a["at"])
    return out


def due(state, now, root=None):
    """Local date (str) when the daily dream alert is due, else None; records state["dream"]["last"] on a hit.
    Due = at or after DREAM_TIME in the owner's zone (meta/settings.tz, else the host zone) and not yet fired
    for that local date. No late limit: a poller that was down fires on its first tick of the day."""
    import morning_brief as mb
    tz = mb.resolve(mb.read_settings(root))["tz"]
    local = datetime.fromtimestamp(now, timezone.utc).astimezone(tz)
    h, m = DREAM_TIME
    if (local.hour, local.minute) < (h, m):
        return None
    mark = state.setdefault("dream", {})
    if mark.get("last") == local.date().isoformat():
        return None
    mark["last"] = local.date().isoformat()
    return {"date": local.date().isoformat(), "time": f"{h:02d}:{m:02d}", "tz": str(tz)}


def render(day, since, prompts, dropped, answers):
    n = len(answers)
    counts = {k: sum(1 for a in answers if a["kind"] == k) for k in ("rec", "other", "free")}
    picked = counts["rec"] + counts["other"]
    pct = lambda a, b: f"{100 * a / b:.0f}%" if b else "n/a"  # noqa: E731
    lines = [f"# Dream digest {day}", "",
             f"Since: {since.strftime('%Y-%m-%dT%H:%MZ') if since else 'the beginning'} (default: last owner/dream-log.md entry).",
             "", "## Owner prompts", "", f"Count: {len(prompts)} (credential-like prompts dropped: {dropped}).", ""]
    lines += [f"- {ts.strftime('%m-%dT%H:%MZ')} {' '.join(t.split())}" for ts, t in prompts]
    lines += ["", "## Answered decisions", "",
              f"Count: {n}. took-rec {counts['rec']}, other option {counts['other']}, free-text {counts['free']}.",
              f"Uptake (took-rec / all answers): {pct(counts['rec'], n)}. Of option picks only: {pct(counts['rec'], picked)}.",
              "", "## Overrides (other option or free text)", ""]
    for a in answers:
        if a["kind"] != "rec":
            lines.append(f"- {a['id']} [{a['kind']}] {' '.join(a['title'].split())[:120]} -> "
                         f"{' '.join(a['answer'].split())[:200]} (rec: {(a['rec'] or 'none')[:100]})")
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--since", help="ISO UTC override; default the last owner/dream-log.md timestamp")
    ap.add_argument("--out", default=os.path.join(ROOT, "local", "briefs", "dream"))
    ap.add_argument("--root", default=os.environ.get("DASHBOARD_ROOT") or os.path.join(ROOT, "local", "owner-queue"))
    ap.add_argument("--transcripts", help="transcript directory; default from local/architect.json")
    ap.add_argument("--log", default=os.path.join(ROOT, "owner", "dream-log.md"))
    args = ap.parse_args(argv)
    since = parse_ts(args.since) if args.since else last_logged(args.log)
    prompts, dropped = owner_prompts(args.transcripts or default_transcripts(), since)
    answers = answered_items(args.root, since)
    day = datetime.now().astimezone().strftime("%Y-%m-%d")
    os.makedirs(args.out, exist_ok=True)
    path = os.path.join(args.out, f"{day}-digest.md")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(render(day, since, prompts, dropped, answers))
    n = len(answers)
    rec = sum(1 for a in answers if a["kind"] == "rec")
    print(f"digest={path} prompts={len(prompts)} dropped_cred={dropped} answers={n} took_rec={rec} "
          f"other={sum(1 for a in answers if a['kind'] == 'other')} free={sum(1 for a in answers if a['kind'] == 'free')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
