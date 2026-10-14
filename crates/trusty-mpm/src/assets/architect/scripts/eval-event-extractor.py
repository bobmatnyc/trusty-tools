#!/usr/bin/env python3
"""eval-event-extractor.py - score the event-extractor agent on real PM messages (T07).

Why: the Architect hands PM messages to a cheap-model agent
  (.claude/agents/event-extractor.md). The pass bar from D6b is 100% on SHAs and
  PR numbers and at least 90% on kind, or the agent moves from haiku to sonnet.
What: for each fixture in scripts/fixtures/event-extractor.jsonl
  ({id, from, text, expected{kind, repo, pr, head, base, merge_sha, version}})
  it makes a scratch work dir (empty inbox/, scripts/ linked to this checkout,
  a copy of the agent definition with the model replaced by --model), runs
  `claude -p --agent event-extractor` there with VERIFY_CLAIM_OFFLINE=1, reads
  the event line the agent appended to the scratch inbox/events.jsonl and
  compares fields. The live inbox/events.jsonl is never touched.
  Prints one line per fixture, accuracy per field, the count of SHAs the agent
  wrote that are not in the message text, the bar verdict and token use per run. Exit 0 = bar met, 1 = bar missed, 2 = bad usage or no results.
  --score-only FILE re-scores saved results (the --out file) without running.
Test: scripts/test_eval_event_extractor.py
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(ROOT, "scripts", "fixtures", "event-extractor.jsonl")
AGENT = os.path.join(ROOT, ".claude", "agents", "event-extractor.md")
SCORED = ("head", "base", "merge_sha", "pr", "version", "kind")
EXACT_FIELDS = ("head", "base", "merge_sha", "pr")  # bar: 100%
KIND_BAR = 0.90


def load_fixtures(path: str = FIXTURES) -> list[dict]:
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def norm(field: str, value: object) -> object:
    """Comparable form: SHAs lower case, pr as int, empty string as null."""
    if value in (None, ""):
        return None
    if field == "pr":
        try:
            return int(value)
        except (TypeError, ValueError):
            return str(value)
    return str(value).strip().lower() if field in ("head", "base", "merge_sha") else str(value).strip()


def score_event(expected: dict, got: dict | None, fields=SCORED + ("repo",)) -> dict:
    """field -> True/False; a missing event fails every field."""
    return {f: got is not None and norm(f, expected.get(f)) == norm(f, got.get(f)) for f in fields}


def fabricated(text: str, got: dict | None) -> list[str]:
    """head/base/merge_sha values that do not appear in the message (invented SHAs)."""
    if not got:
        return []
    t = text.lower()
    return [f for f in ("head", "base", "merge_sha")
            if got.get(f) and str(got[f]).lower() not in t]


def summarize(rows: list[dict]) -> dict:
    """Accuracy per field and the D6b bar verdict from per-fixture score dicts."""
    n = len(rows)
    acc = {f: (sum(r["score"][f] for r in rows) / n if n else 0.0) for f in SCORED + ("repo",)}
    exact = [r["score"][f] for r in rows for f in EXACT_FIELDS]
    exact_acc = sum(exact) / len(exact) if exact else 0.0
    return {"n": n, "field_accuracy": acc, "sha_pr_accuracy": exact_acc,
            "kind_accuracy": acc["kind"],
            "pass": bool(rows) and exact_acc == 1.0 and acc["kind"] >= KIND_BAR}


def agent_text(model: str | None) -> str:
    with open(AGENT) as f:
        text = f.read()
    if model:
        text, n = re.subn(r"(?m)^model:.*$", f"model: {model}", text, count=1)
        if n != 1:
            raise SystemExit("agent definition has no model: line")
    return text


def usage_total(result: dict) -> dict:
    u = result.get("usage") or {}
    inp = sum(u.get(k, 0) or 0 for k in
              ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))
    return {"input": inp, "output": u.get("output_tokens", 0) or 0,
            "cost_usd": result.get("total_cost_usd"), "turns": result.get("num_turns"),
            "models": sorted((result.get("modelUsage") or {}).keys())}


def run_one(fx: dict, model: str | None, timeout: int) -> dict:
    work = tempfile.mkdtemp(prefix=f"evalx-{fx['id']}-")
    try:
        os.makedirs(os.path.join(work, "inbox"))
        os.makedirs(os.path.join(work, ".claude", "agents"))
        os.symlink(os.path.join(ROOT, "scripts"), os.path.join(work, "scripts"))
        with open(os.path.join(work, ".claude", "agents", "event-extractor.md"), "w") as f:
            f.write(agent_text(model))
        prompt = f"FROM: {fx['from']}\n\n{fx['text']}"
        cmd = ["claude", "-p", prompt, "--agent", "event-extractor", "--output-format", "json",
               "--permission-mode", "bypassPermissions", "--no-session-persistence"]
        env = dict(os.environ, VERIFY_CLAIM_OFFLINE="1")
        row = {"id": fx["id"], "expected": fx["expected"], "event": None, "reply": "", "error": None}
        try:
            p = subprocess.run(cmd, cwd=work, env=env, capture_output=True, text=True, timeout=timeout)
            result = json.loads(p.stdout) if p.stdout.strip() else {}
            row["reply"] = str(result.get("result", ""))
            row["usage"] = usage_total(result)
            if p.returncode != 0 or result.get("is_error"):
                row["error"] = (p.stderr.strip() or row["reply"])[:200]
        except (subprocess.TimeoutExpired, ValueError, OSError) as e:
            row["error"] = f"{type(e).__name__}: {e}"[:200]
        ev_path = os.path.join(work, "inbox", "events.jsonl")
        if os.path.exists(ev_path):
            lines = [ln for ln in open(ev_path).read().splitlines() if ln.strip()]
            if lines:
                try:
                    row["event"] = json.loads(lines[-1])
                except ValueError:
                    row["error"] = row["error"] or "last event line is not JSON"
        row["score"] = score_event(fx["expected"], row["event"])
        row["fabricated"] = fabricated(fx["text"], row["event"])
        return row
    finally:
        shutil.rmtree(work, ignore_errors=True)


def report(rows: list[dict], label: str) -> dict:
    for r in rows:
        r.setdefault("score", score_event(r["expected"], r.get("event")))
    s = summarize(rows)
    print(f"== event-extractor eval: {label}, {s['n']} messages")
    for r in rows:
        bad = [f for f, ok in r["score"].items() if not ok]
        print(f"{r['id']} {'ok  ' if not bad else 'MISS'} {', '.join(bad)}" + (f"  [{r['error']}]" if r.get("error") else ""))
        for f in bad:
            got = (r.get("event") or {}).get(f)
            print(f"      {f}: expected {r['expected'].get(f)!r}, got {got!r}")
    fab = sum(len(r.get("fabricated") or []) for r in rows)
    print(f"SHAs not found in the message text (invented): {fab}")
    print("field accuracy: " + ", ".join(f"{f} {s['field_accuracy'][f]:.0%}" for f in SCORED + ("repo",)))
    print(f"SHA+PR (head, base, merge_sha, pr) {s['sha_pr_accuracy']:.0%} (bar 100%); "
          f"kind {s['kind_accuracy']:.0%} (bar {KIND_BAR:.0%})")
    use = [r["usage"] for r in rows if r.get("usage")]
    if use:
        print(f"tokens per run (mean): input {sum(u['input'] for u in use) // len(use)}, "
              f"output {sum(u['output'] for u in use) // len(use)}; "
              f"cost ${sum(u['cost_usd'] or 0 for u in use) / len(use):.4f}; "
              f"models {sorted({m for u in use for m in u['models']})}")
    print("VERDICT: " + ("PASS" if s["pass"] else "FAIL"))
    return s


def main(argv: list | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", help="replace the agent's model for this run (haiku, sonnet)")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--ids", help="comma-separated fixture ids")
    ap.add_argument("--fixtures", default=FIXTURES)
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--out", help="save per-fixture results as JSON")
    ap.add_argument("--score-only", metavar="FILE", help="re-score a saved --out file")
    a = ap.parse_args(argv)
    if a.score_only:
        with open(a.score_only) as f:
            rows = json.load(f)
        return 0 if report(rows, f"saved {a.score_only}")["pass"] else 1
    fixtures = load_fixtures(a.fixtures)
    if a.ids:
        want = set(a.ids.split(","))
        fixtures = [f for f in fixtures if f["id"] in want]
    if not fixtures:
        print("no fixtures selected", file=sys.stderr)
        return 2
    with ThreadPoolExecutor(max_workers=max(1, a.jobs)) as ex:
        rows = list(ex.map(lambda fx: run_one(fx, a.model, a.timeout), fixtures))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(rows, f, indent=1)
    return 0 if report(rows, a.model or "agent default model")["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
