#!/usr/bin/env python3
"""pm_context_probe.py — first-turn context size of a throwaway PM, per lever (#8998).

Why: the release-1 target is a PM first turn of 70k input tokens or less. To
  know which source to cut, each source must be switched off on its own and
  the first-turn size read back. The 10-01 scratch scripts that did this are
  gone, so this probe is committed and re-run on main.

What: `run` launches a throwaway headless PM (`claude -p`) with the CURRENT
  launch flags (stated once, in LAUNCH below), once per lever, and records the
  first API call's input tokens (input + cache creation + cache read) from the
  stream-json usage. Levers:
    baseline           the current main launch for a project with no owner file
    no-prompt          drops --append-system-prompt-file
    mcp-off            --strict-mcp-config, no --mcp-config: no MCP server loads
    connectors-off     --settings {"disableClaudeAiConnectors": true}
    skills-off         --settings skillOverrides: every user-tier skill "off"
    agents-off         --settings permissions.deny: Agent(<name>), Task(<name>) per agent
    skills-agents-off  skills-off + agents-off
    all-trims          mcp-off + connectors-off + skills-off + agents-off
    catchup            baseline; the PM calls session_context_catchup for
                       --catchup-project and the call after the result is read
  The --settings levers use the same subtractive keys the #8988/#8992 owner
  policy writes (crates/trusty-mpm/src/core/launch_policy/carrier.rs).
  `parse` reads one saved stream-json file; `report` renders a results JSON as
  a markdown table; `print-env` shows the child environment's variable names.

  Safety: the child environment is an allowlist (ENV_ALLOWLIST) plus LAUNCH's
  variables, so no token variable is inherited or set; auth is whatever the
  CLAUDE_CONFIG_DIR already holds. The probe reads no credential store. Every
  run uses one git-initialised project in a fresh temp directory and its own
  process group, which is killed and checked empty afterwards. At the end the
  temp tree and the project's transcript directory under
  CLAUDE_CONFIG_DIR/projects are removed and `ps` is checked for any process
  naming the temp tree. No tmux is used. Results: pm_context_probe_baseline.md.

Usage:
  scripts/pm_context_probe.py run [--runs 3] [--lever-runs 1] [--levers a,b]
      [--tm PATH] [--model M] [--catchup-project DIR] [--out results.json]
  scripts/pm_context_probe.py parse <stream.jsonl>
  scripts/pm_context_probe.py report <results.json>
Exit: 0 on success; 1 when a run or a parse fails (the raw error is printed);
  2 on a bad invocation or a refused environment.
Test: scripts/pm_context_probe_selftest.sh.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# The current main PM launch, stated in one place. Each entry names its source.
LAUNCH = {
    # runtime/managed_launch.rs isolation_args + core/model_inject.rs:172
    # (SETTING_SOURCES_FLAG_RELOCATED: CLAUDE_CONFIG_DIR is relocated).
    "setting_sources": ["--setting-sources", "user,project,local"],
    # core/pm_tool_trim.rs:25,36 (#8992), PM profile only.
    "disallowed_tools": ["--disallowedTools", "Workflow,DesignSync"],
    # core/model_inject.rs:264.
    "permission_flag": ["--dangerously-skip-permissions"],
    # core/mcp_config.rs:74,164: the builtins the composed --mcp-config adds.
    "builtin_mcp": {
        "trusty-memory": ["serve", "--stdio"],
        "trusty-mpm": ["serve", "--stdio"],
        "trusty-review": ["serve", "--stdio"],
        "trusty-search": ["serve"],
    },
    # runtime/managed_launch.rs managed_env_set + base(); core/pm_tool_trim.rs:22;
    # core/session_profile.rs:37,56. The OAuth token tm also sets is NOT here.
    "env": {
        "CLAUDE_CODE_ENABLE_TODO_TOOLS": "1",
        "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
        "CLAUDE_CODE_DISABLE_ARTIFACT": "1",
        "TRUSTY_MPM_SESSION_PROFILE": "pm",
    },
    # No --settings: the carrier writes one only when an owner file
    # (<repos_root>/<owner>/.trusty-mpm-owner.toml) applies, and a temp
    # directory outside the repos root with no origin has no owner.
    # No --model: the managed spawn passes none; the user tier picks it.
}

DEFAULT_CONFIG_DIR = Path.home() / ".trusty-tools" / "trusty-mpm" / "claude-config"
ENV_ALLOWLIST = ("PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "TMPDIR")
SECRET_NAME = re.compile(r"TOKEN|SECRET|PASSW|API_?KEY|CREDENTIAL|_KEY$|^GH_|^GITHUB_", re.I)
REFUSED_ROOTS = ("Documents", "Desktop")
FIRST_TURN_PROMPT = "Reply with the single word OK. Do not call any tool."
CATCHUP_TOOL = "session_context_catchup"
CATCHUP_PROMPT = (
    "Call the MCP tool {tool} exactly once with project_dir set to \"{project}\" "
    "(load it with ToolSearch first if it is deferred). Call no other tool. Then "
    "reply with the single word DONE."
)
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
}


class ProbeError(Exception):
    """A run or a parse that produced no usable number; the message is the raw cause."""


def child_env(parent: dict[str, str], config_dir: Path) -> dict[str, str]:
    """The allowlisted child environment plus LAUNCH's variables.

    What: copies only ENV_ALLOWLIST names from `parent`, then sets
    CLAUDE_CONFIG_DIR and LAUNCH["env"]. Raises ProbeError when any resulting
    name looks like a credential, so a future allowlist edit cannot leak one.
    """
    env = {k: parent[k] for k in ENV_ALLOWLIST if k in parent}
    env["TERM"] = "dumb"
    env["CLAUDE_CONFIG_DIR"] = str(config_dir)
    env.update(LAUNCH["env"])
    leaked = sorted(k for k in env if SECRET_NAME.search(k))
    if leaked:
        raise ProbeError(f"refusing: credential-like variables in child env: {leaked}")
    return env


def check_root(path: Path) -> None:
    """Refuse a throwaway root under ~/Documents or ~/Desktop (macOS TCC folders)."""
    resolved = path.resolve()
    for name in REFUSED_ROOTS:
        guarded = (Path.home() / name).resolve()
        if resolved == guarded or guarded in resolved.parents:
            raise ProbeError(f"refusing to run under {guarded}: {resolved}")


def frontmatter_name(md: Path) -> str | None:
    """The `name:` value of a Markdown file's leading YAML frontmatter, if any."""
    try:
        lines = md.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return None
    if not lines or lines[0].strip() != "---":
        return None
    for line in lines[1:]:
        if line.strip() == "---":
            return None
        m = re.match(r"name:\s*['\"]?([^'\"#]+?)['\"]?\s*(#.*)?$", line)
        if m:
            return m.group(1).strip()
    return None


def tier_names(config_dir: Path) -> tuple[list[str], list[str]]:
    """User-tier skill and agent names under `config_dir` (names only, sorted)."""
    skills: set[str] = set()
    for sk in sorted((config_dir / "skills").glob("*/SKILL.md")):
        skills.add(frontmatter_name(sk) or sk.parent.name)
    agents: set[str] = set()
    for ag in sorted((config_dir / "agents").glob("*.md")):
        agents.add(frontmatter_name(ag) or ag.stem)
    return sorted(skills), sorted(agents)


def settings_body(parts: tuple[str, ...], skills: list[str], agents: list[str]) -> dict:
    """The --settings JSON for a lever's parts (subtractive keys only)."""
    body: dict = {}
    if "connectors" in parts:
        body["disableClaudeAiConnectors"] = True
    if "skills" in parts:
        body["skillOverrides"] = {name: "off" for name in skills}
    if "agents" in parts:
        # The subagent tool is `Task` in some Claude Code builds, `Agent` in others.
        body["permissions"] = {"deny": [f"{tool}({name})" for name in agents for tool in ("Agent", "Task")]}
    return body


def build_argv(lever: str, prompt_file: Path | None, mcp_file: Path,
               settings_file: Path | None, model: str | None, user_prompt: str,
               max_turns: int) -> list[str]:
    """The claude argv for `lever`, in the order the real launch emits it."""
    drop_prompt, strict_mcp, _parts, _catchup = LEVERS[lever]
    argv = ["claude", "-p", user_prompt, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns), "--no-session-persistence"]
    if model:
        argv += ["--model", model]
    if prompt_file is not None and not drop_prompt:
        argv += ["--append-system-prompt-file", str(prompt_file)]
    argv += LAUNCH["setting_sources"]
    argv += ["--strict-mcp-config"] if strict_mcp else ["--mcp-config", str(mcp_file)]
    if settings_file is not None:
        argv += ["--settings", str(settings_file)]
    argv += LAUNCH["disallowed_tools"] + LAUNCH["permission_flag"]
    return argv


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
    }


def reap_group(pgid: int, grace: float = 5.0) -> list[int]:
    """Kill process group `pgid`; return the pids still in it afterwards (empty = clean)."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pgid, sig)
        except ProcessLookupError:
            return []
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            try:
                os.killpg(pgid, 0)
            except ProcessLookupError:
                return []
            time.sleep(0.2)
    out = subprocess.run(["ps", "-axo", "pid=,pgid="], capture_output=True, text=True).stdout
    return [int(p) for p, g in (l.split() for l in out.splitlines() if l.strip()) if int(g) == pgid]


def project_slug(path: Path) -> str:
    """Claude Code's transcript directory name for a cwd."""
    return re.sub(r"[^A-Za-z0-9]", "-", str(path.resolve()))


def one_run(lever: str, args, env: dict, root: Path, prompt_file: Path, mcp_file: Path,
            names: tuple[list[str], list[str]], index: int) -> dict:
    """Launch one throwaway PM for `lever` in `root/project`; return its numbers or the error."""
    parts = LEVERS[lever][2]
    settings_file = None
    if parts:
        settings_file = root / f"settings-{lever}.json"
        settings_file.write_text(json.dumps(settings_body(parts, *names), indent=1))
    catchup = LEVERS[lever][3]
    prompt = (CATCHUP_PROMPT.format(tool=CATCHUP_TOOL, project=args.catchup_project)
              if catchup else FIRST_TURN_PROMPT)
    argv = build_argv(lever, prompt_file, mcp_file, settings_file, args.model, prompt,
                      6 if catchup else 1)
    stream, errlog = root / f"{lever}-{index}.jsonl", root / f"{lever}-{index}.err"
    record = {"lever": lever, "run": index}
    with open(stream, "w") as out, open(errlog, "w") as err:
        child = subprocess.Popen(argv, cwd=root / "project", env=env, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=err, start_new_session=True)
        try:
            child.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            record["error"] = f"timed out after {args.timeout}s"
        finally:
            record["leaked_pids"] = reap_group(child.pid)
    try:
        with open(stream) as fh:
            record.update(parse_stream(fh))
    except ProbeError as exc:
        tail = errlog.read_text(errors="replace")[-800:]
        record["error"] = f"{record.get('error', '')} {exc}; exit={child.returncode}; stderr: {tail}".strip()
    return record


def prepare(root: Path, args, env: dict) -> tuple[Path, Path]:
    """The throwaway project (git repo + tm-composed PM prompt) and the builtin MCP file."""
    proj = root / "project"
    proj.mkdir()
    subprocess.run(["git", "init", "-q", str(proj)], check=True, env=env)
    prompt = subprocess.run([args.tm, "session", "instructions", "--dir", str(proj)], cwd=proj, env=env,
                            stdin=subprocess.DEVNULL, capture_output=True, text=True)
    if prompt.returncode != 0 or not prompt.stdout.strip():
        raise ProbeError(f"`{args.tm} session instructions` failed ({prompt.returncode}): {prompt.stderr[-800:]}")
    prompt_file = root / "pm-system-prompt.txt"
    prompt_file.write_text(prompt.stdout)
    servers = {name: {"type": "stdio", "command": name, "args": a}
               for name, a in LAUNCH["builtin_mcp"].items()}
    mcp_file = root / "session-mcp.json"
    mcp_file.write_text(json.dumps({"mcpServers": servers}, indent=1))
    return prompt_file, mcp_file


def stray_processes(root: Path) -> list[str]:
    """Live processes whose command line names `root` (each run's claude does)."""
    out = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
    return [line.strip()[:200] for line in out.splitlines() if str(root) in line]


def cmd_run(args) -> int:
    config_dir = Path(args.config_dir)
    levers = args.levers.split(",")
    unknown = [lv for lv in levers if lv not in LEVERS]
    if unknown or not config_dir.is_dir():
        print(f"pm_context_probe: unknown lever(s) {unknown} (known: {list(LEVERS)}) "
              f"or no CLAUDE_CONFIG_DIR at {config_dir}", file=sys.stderr)
        return 2
    try:
        env = child_env(dict(os.environ), config_dir)
        root = Path(tempfile.mkdtemp(prefix="pm-context-probe-"))
        check_root(root)
    except ProbeError as exc:
        print(f"pm_context_probe: {exc}", file=sys.stderr)
        return 2
    records, failed = [], False
    try:
        prompt_file, mcp_file = prepare(root, args, env)
        names = tier_names(config_dir)
        meta = {"tm": subprocess.run([args.tm, "--version"], capture_output=True, text=True, env=env).stdout.strip(),
                "claude": subprocess.run(["claude", "--version"], capture_output=True, text=True, env=env).stdout.strip(),
                "prompt_bytes": prompt_file.stat().st_size,
                "user_tier_skills": len(names[0]), "user_tier_agents": len(names[1]),
                "date_utc": time.strftime("%Y-%m-%dT%H:%MZ", time.gmtime()),
                "catchup_project": args.catchup_project, "launch": LAUNCH}
        for lever in levers:
            for i in range(args.runs if lever == "baseline" else args.lever_runs):
                rec = one_run(lever, args, env, root, prompt_file, mcp_file, names, i + 1)
                failed |= "error" in rec or bool(rec["leaked_pids"])
                print(json.dumps({k: rec.get(k) for k in ("lever", "run", "first_turn", "post_catchup", "error", "leaked_pids")}),
                      file=sys.stderr)
                records.append(rec)
    except ProbeError as exc:
        print(f"pm_context_probe: {exc}", file=sys.stderr)
        return 1
    finally:
        shutil.rmtree(config_dir / "projects" / project_slug(root / "project"), ignore_errors=True)
        meta_stray = stray_processes(root)
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
    meta["stray_processes"] = meta_stray
    failed |= bool(meta_stray)
    results = {"meta": meta, "runs": records}
    if args.out:
        Path(args.out).write_text(json.dumps(results, indent=1) + "\n")
    print(render_report(results))
    print(f"stray processes after cleanup: {meta_stray or 'none'}", file=sys.stderr)
    return 1 if failed else 0


def render_report(results: dict) -> str:
    """Markdown table: one row per lever (median of runs) and the catchup rows."""
    by: dict[str, list[dict]] = {}
    for rec in results["runs"]:
        by.setdefault(rec["lever"], []).append(rec)
    ok = lambda recs, key: [r[key] for r in recs if r.get(key) is not None and "error" not in r]
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
            rows.append(f"| {lever} | - | 0/{len(recs)} | - | failed: {recs[0].get('error', '')[:120]} |")
            continue
        med = statistics.median(vals)
        delta = f"{med - base_med:+,.0f}" if base_med is not None else "-"
        rows.append(f"| {lever} | {med:,.0f} | {', '.join(f'{v:,}' for v in vals)} | {delta} | measured |")
    post = ok(by.get("catchup", []), "post_catchup")
    if post:
        first = ok(by.get("catchup", []), "first_turn")
        med = statistics.median(post)
        delta = f"{med - base_med:+,.0f}" if base_med is not None else "-"
        rows.append(f"| post-{CATCHUP_TOOL} | {med:,.0f} | {', '.join(f'{v:,}' for v in post)} | {delta} | measured |")
        rows.append(f"| catchup payload (post - first turn, same run) | {med - statistics.median(first):,.0f} | - | - | inferred |")
    return "\n".join(rows)


def main_checkout() -> str:
    """The main checkout of this repository (a worktree's sessions live there)."""
    here = Path(__file__).resolve().parent
    out = subprocess.run(["git", "-C", str(here), "rev-parse", "--path-format=absolute",
                          "--git-common-dir"], capture_output=True, text=True)
    return str(Path(out.stdout.strip()).parent) if out.returncode == 0 else str(here.parent)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    run = sub.add_parser("run")
    run.add_argument("--runs", type=int, default=3, help="baseline runs")
    run.add_argument("--lever-runs", type=int, default=1, help="runs per other lever")
    run.add_argument("--levers", default=",".join(LEVERS))
    run.add_argument("--tm", default="tm", help="tm binary composing the PM prompt")
    run.add_argument("--model", default=None)
    run.add_argument("--config-dir", default=os.environ.get("PM_PROBE_CONFIG_DIR", str(DEFAULT_CONFIG_DIR)))
    run.add_argument("--catchup-project", default=None, help="default: this repo's main checkout")
    run.add_argument("--timeout", type=int, default=300)
    run.add_argument("--out")
    run.add_argument("--keep", action="store_true", help="keep the temp tree for debugging")
    parse = sub.add_parser("parse")
    parse.add_argument("stream")
    report = sub.add_parser("report")
    report.add_argument("results")
    penv = sub.add_parser("print-env")
    penv.add_argument("--config-dir", default=str(DEFAULT_CONFIG_DIR))
    args = ap.parse_args(argv)
    try:
        if args.cmd == "run":
            args.catchup_project = args.catchup_project or main_checkout()
            return cmd_run(args)
        if args.cmd == "parse":
            with open(args.stream) as fh:
                print(json.dumps(parse_stream(fh)))
            return 0
        if args.cmd == "report":
            print(render_report(json.loads(Path(args.results).read_text())))
            return 0
        print("\n".join(sorted(child_env(dict(os.environ), Path(args.config_dir)))))
        return 0
    except ProbeError as exc:
        print(f"pm_context_probe: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
