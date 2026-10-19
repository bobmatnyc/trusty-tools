"""pm_context_probe_readout.py - lever table and stream readout for pm_context_probe.py (#8998).

Why: pm_context_probe.py hit the 500-SLOC cap; the lever table and the
  transcript readout are the part with no process or filesystem side effects,
  so they split off cleanly.
What: LEVERS and PROJECT_LEVERS, ProbeError, parse_stream (stream-json to
  token counts), run_problem (fail-closed run check) and render_report
  (markdown table). pm_context_probe.py re-exports every name.
Test: scripts/pm_context_probe_selftest.sh.
"""

from __future__ import annotations

import json
import statistics

CATCHUP_TOOL = "session_context_catchup"
LEVERS = {
    # name: (drop_prompt, strict_mcp, settings parts, catchup)
    "baseline": (False, False, (), False),
    "no-prompt": (True, False, (), False),
    "mcp-off": (False, True, (), False),
    "connectors-off": (False, False, ("connectors",), False),
    "skills-off": (False, False, ("skills",), False),
    "agents-off": (False, False, ("agents",), False),
    "skills-agents-off": (False, False, ("skills", "agents"), False),
    "all-trims": (False, True, ("connectors", "skills", "agents"), False),
    "catchup": (False, False, (), True),
    # Project-tier levers (#8998 condition 2); they need --project-dir.
    "claude-md-off": (False, False, ("claude-md",), False),
    "project-skills-off": (False, False, ("project-skills",), False),
    "project-files-off": (False, False, ("claude-md", "project-skills"), False),
}
PROJECT_LEVERS = ("claude-md-off", "project-skills-off", "project-files-off")


class ProbeError(Exception):
    """A run or a parse that produced no usable number; the message is the raw cause."""



def usage_tokens(usage: dict) -> int:
    """Context size of one API call: input + cache creation + cache read."""
    return sum(int(usage.get(k) or 0) for k in
               ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))


def parse_stream(lines) -> dict:
    """Read a `claude -p --output-format stream-json --verbose` transcript.

    What: one entry per distinct assistant message id, in order, sized by
    usage_tokens. `first_turn` is the first call; `post_catchup` is the first
    call after the tool_result answering a *session_context_catchup tool_use.
    Raises ProbeError when no assistant call carries usage, or when the
    catchup tool returned an error, quoting the result event's text.
    """
    init, result, calls, seen = None, None, [], set()
    catchup_ids: set[str] = set()
    catchup_after = None
    for raw in lines:
        try:
            obj = json.loads(raw)
        except ValueError:
            continue
        kind = obj.get("type")
        if kind == "system" and obj.get("subtype") == "init":
            init = obj
        elif kind == "assistant":
            msg = obj.get("message") or {}
            mid = msg.get("id")
            if msg.get("usage") and mid not in seen:
                seen.add(mid)
                calls.append(usage_tokens(msg["usage"]))
            for block in msg.get("content") or []:
                if block.get("type") == "tool_use" and str(block.get("name", "")).endswith(CATCHUP_TOOL):
                    catchup_ids.add(block.get("id"))
        elif kind == "user":
            content = (obj.get("message") or {}).get("content")
            for block in content if isinstance(content, list) else []:
                if block.get("type") == "tool_result" and block.get("tool_use_id") in catchup_ids:
                    if block.get("is_error"):
                        raise ProbeError(f"{CATCHUP_TOOL} failed: {str(block.get('content'))[:500]}")
                    catchup_after = len(calls)
        elif kind == "result":
            result = obj
    if not calls:
        text = (result or {}).get("result") or (result or {}).get("subtype") or "no result event"
        raise ProbeError(f"no assistant call with usage; result: {text}")
    servers = (init or {}).get("mcp_servers") or []
    return {
        "first_turn": calls[0],
        "post_catchup": calls[catchup_after] if catchup_after is not None and catchup_after < len(calls) else None,
        "calls": calls,
        "model": (init or {}).get("model"),
        "tools": len((init or {}).get("tools") or []),
        "mcp_servers": sorted(f"{s.get('name')}:{s.get('status')}" for s in servers),
        "result_is_error": bool((result or {}).get("is_error")),
        "result_text": str((result or {}).get("result") or "")[:300],
    }


def run_problem(rec: dict) -> str | None:
    """Why a run must not count as measured, or None when it may.

    What: an existing `error`; a non-zero `exit_code` (absent = unknown, as in
    a parsed stream); `result_is_error`; a missing or non-positive
    `first_turn`; or a catchup run with no `post_catchup`. #8998 review: the
    readout fails closed, so a failed run can never become a baseline row.
    """
    if rec.get("error"):
        return str(rec["error"])
    if rec.get("exit_code") not in (None, 0):
        return f"claude exited {rec['exit_code']}"
    if rec.get("result_is_error"):
        return f"result is_error: {str(rec.get('result_text') or '')[:300]}".rstrip(": ")
    if not isinstance(rec.get("first_turn"), int) or rec["first_turn"] <= 0:
        return f"first turn is {rec.get('first_turn')}"
    if rec.get("lever") == "catchup" and rec.get("post_catchup") is None:
        return f"catchup tool not called: no API call followed a {CATCHUP_TOOL} result"
    return None


def render_report(results: dict) -> str:
    """Markdown table: one row per lever (median of runs) and the catchup rows."""
    by: dict[str, list[dict]] = {}
    for rec in results["runs"]:
        by.setdefault(rec["lever"], []).append(rec)
    ok = lambda recs, key: [r[key] for r in recs if r.get(key) is not None and run_problem(r) is None]
    base = ok(by.get("baseline", []), "first_turn")
    base_med = statistics.median(base) if base else None
    rows = ["| Lever | First-turn input tokens (median) | Runs | Delta vs baseline | Status |",
            "|---|---|---|---|---|"]
    for lever in LEVERS:
        recs = by.get(lever)
        if not recs:
            continue
        vals = ok(recs, "first_turn")
        if not vals:
            rows.append(f"| {lever} | - | 0/{len(recs)} | - | failed: {(run_problem(recs[0]) or '')[:120]} |")
            continue
        med = statistics.median(vals)
        delta = f"{med - base_med:+,.0f}" if base_med is not None else "-"
        status = "measured" if len(vals) == len(recs) else f"measured ({len(recs) - len(vals)} of {len(recs)} runs failed)"
        rows.append(f"| {lever} | {med:,.0f} | {', '.join(f'{v:,}' for v in vals)} | {delta} | {status} |")
    post = ok(by.get("catchup", []), "post_catchup")
    if post:
        first = ok(by.get("catchup", []), "first_turn")
        med = statistics.median(post)
        delta = f"{med - base_med:+,.0f}" if base_med is not None else "-"
        rows.append(f"| post-{CATCHUP_TOOL} | {med:,.0f} | {', '.join(f'{v:,}' for v in post)} | {delta} | measured |")
        rows.append(f"| catchup payload (post - first turn, same run) | {med - statistics.median(first):,.0f} | - | - | inferred |")
    return "\n".join(rows)
