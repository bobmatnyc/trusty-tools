#!/usr/bin/env python3
"""Startup tokens per subagent type, from Claude Code transcripts.

Why: the Architect pays for every subagent start; this measures it per agent type.
What: finds each subagent's first assistant turn in <dir>/*.jsonl (sidechain lines,
grouped by agentId) and <dir>/*/subagents/agent-*.jsonl, takes input_tokens +
cache_creation_input_tokens + cache_read_input_tokens, and prints count, median and
max per agent type. The type comes from the sibling agent-*.meta.json ("agentType");
when it is absent the type is "unknown (no meta)", never guessed.
Test: scripts/test_subagent_startup_tokens.py
Usage: subagent-startup-tokens.py [transcript_dir]
"""
import glob
import json
import os
import re
import statistics
import sys

# This checkout's Claude Code transcript dir: its path with every non-alphanumeric -> "-".
DEFAULT = os.path.join(os.path.expanduser("~/.trusty-tools/trusty-mpm/claude-config/projects"),
                       re.sub(r"[^A-Za-z0-9]", "-", os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
UNKNOWN = "unknown (no meta)"


def startup(usage):
    return sum(int(usage.get(k) or 0) for k in (
        "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"))


def first_turns(path):
    """Yield (agentId or None, startup tokens) for the first assistant turn per agent."""
    seen = set()
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            msg = rec.get("message")
            if rec.get("type") != "assistant" or not isinstance(msg, dict):
                continue
            usage = msg.get("usage")
            if not isinstance(usage, dict):
                continue
            aid = rec.get("agentId") if rec.get("isSidechain") else None
            if aid in seen:
                continue
            seen.add(aid)
            yield aid, startup(usage)


def agent_type(meta_path):
    try:
        with open(meta_path, encoding="utf-8") as fh:
            return json.load(fh).get("agentType") or UNKNOWN
    except (OSError, ValueError):
        return UNKNOWN


def collect(root):
    """Return {agent type: [startup tokens]}."""
    out = {}
    for path in sorted(glob.glob(os.path.join(root, "*", "subagents", "agent-*.jsonl"))):
        kind = agent_type(path[:-len(".jsonl")] + ".meta.json")
        for _aid, tokens in first_turns(path):
            out.setdefault(kind, []).append(tokens)
    for path in sorted(glob.glob(os.path.join(root, "*.jsonl"))):
        for aid, tokens in first_turns(path):
            if aid is not None:  # sidechain lines inside a main transcript
                out.setdefault(UNKNOWN, []).append(tokens)
    return out


def main(argv):
    root = argv[1] if len(argv) > 1 else DEFAULT
    data = collect(root)
    print(f"{'agent type':<24}{'count':>6}{'median':>10}{'max':>10}")
    for kind, vals in sorted(data.items(), key=lambda kv: -statistics.median(kv[1])):
        print(f"{kind:<24}{len(vals):>6}{int(statistics.median(vals)):>10}{max(vals):>10}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
