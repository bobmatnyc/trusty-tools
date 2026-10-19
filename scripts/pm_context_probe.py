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
    catchup            baseline; the PM calls session_context_catchup for the
                       throwaway project and the call after the result is read.
                       --catchup-project DIR points it at a real checkout
                       instead: opt-in, because the handler can fetch session
                       refs and write the session cache when it is given a
                       session id (crates/trusty-mpm/src/daemon/mcp_context.rs,
                       mcp_context_session_refs.rs).
    claude-md-off      --project-dir only: claudeMdExcludes for the project's CLAUDE.md files
    project-skills-off --project-dir only: skillOverrides "off" for the project-tier skills
    project-files-off  --project-dir only: claude-md-off + project-skills-off together
  --project-dir DIR runs every lever in that existing checkout instead of a
  fresh throwaway project, so the real project's CLAUDE.md, .claude/ and the
  tm-composed prompt for it are loaded. It must be a CLEAN git checkout the
  caller can discard (never a live PM's directory): the probe refuses a dirty
  tree, never initialises, edits or removes it, and re-checks it is still clean
  afterwards. Both checks ignore gitignored paths (`.trusty-mpm/` is ignored). The catchup lever then defaults to DIR itself.
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
  naming the temp tree. SIGTERM and SIGHUP run the same cleanup and exit
  128+signal. No tmux is used. Results: pm_context_probe_baseline.md.

  A run counts as measured only when claude exited 0, the result is not an
  error, the first turn is positive and, for catchup, the tool was called
  (run_problem); anything else is a failed row quoting the raw cause.

Usage:
  scripts/pm_context_probe.py run [--runs 3] [--lever-runs 1] [--levers a,b]
      [--tm PATH] [--model M] [--project-dir DIR] [--catchup-project DIR]
      [--out results.json]
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
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# `python3 -I` drops the script directory from sys.path; the sibling module needs it.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from pm_context_probe_readout import (  # noqa: E402,F401
    CATCHUP_TOOL, LEVERS, PROJECT_LEVERS, ProbeError, parse_stream, render_report, run_problem,
    usage_tokens)

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
CATCHUP_PROMPT = (
    "Call the MCP tool {tool} exactly once with project_dir set to \"{project}\" "
    "(load it with ToolSearch first if it is deferred). Call no other tool. Then "
    "reply with the single word DONE."
)


class Interrupted(Exception):
    """SIGTERM or SIGHUP, raised so the run's `finally` cleanup still executes."""

    def __init__(self, signum: int):
        super().__init__(f"interrupted by signal {signum}")
        self.signum = signum


def raise_interrupted(signum: int, _frame) -> None:
    """Signal handler: ignore further TERM/HUP so cleanup is not cut short, then raise."""
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, signal.SIG_IGN)
    raise Interrupted(signum)


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


def project_tier(project: Path) -> dict:
    """What a real project adds to a PM's context: the files each project lever switches off.

    What: absolute paths of the project's CLAUDE.md files, project-tier skill
    and agent names (`.claude/skills`, `.claude/agents`) and `.mcp.json` server
    names. Names only; nothing is read beyond frontmatter and the `.mcp.json` keys.
    """
    skills, agents = tier_names(project / ".claude")
    mdfiles = [project / "CLAUDE.md", project / "CLAUDE.local.md", project / ".claude" / "CLAUDE.md"]
    servers: list[str] = []
    try:
        servers = sorted((json.loads((project / ".mcp.json").read_text()).get("mcpServers") or {}))
    except (OSError, ValueError):
        pass
    return {"claude_md": [str(f) for f in mdfiles if f.is_file()], "skills": skills,
            "agents": agents, "mcp_servers": servers}


def not_applicable(lever: str, project: dict | None) -> str | None:
    """Why a project lever has nothing to switch off, or None when it has a target.

    What: claude-md-off needs a CLAUDE.md, project-skills-off a project skill,
    project-files-off either. Without a target the run would equal the baseline
    and read as a measured delta of 0, so the caller records a failed row.
    """
    if lever not in PROJECT_LEVERS or project is None:
        return None
    md, sk = bool(project["claude_md"]), bool(project["skills"])
    missing = {"claude-md-off": not md, "project-skills-off": not sk,
               "project-files-off": not (md or sk)}[lever]
    if not missing:
        return None
    what = {"claude-md-off": "CLAUDE.md", "project-skills-off": "skills",
            "project-files-off": "CLAUDE.md or skills"}[lever]
    return f"not applicable (project has no {what})"


def settings_body(parts: tuple[str, ...], skills: list[str], agents: list[str],
                  project: dict | None = None) -> dict:
    """The --settings JSON for a lever's parts (subtractive keys only)."""
    body: dict = {}
    if project is not None and "claude-md" in parts:
        body["claudeMdExcludes"] = list(project["claude_md"])
    if project is not None and "project-skills" in parts:
        body.setdefault("skillOverrides", {}).update({name: "off" for name in project["skills"]})
    if "connectors" in parts:
        body["disableClaudeAiConnectors"] = True
    if "skills" in parts:
        body.setdefault("skillOverrides", {}).update({name: "off" for name in skills})
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


def group_gone(pgid: int) -> bool:
    """True when no signallable process is left in group `pgid`.

    macOS answers EPERM, not ESRCH, for a group whose only members are zombies.
    """
    try:
        os.killpg(pgid, 0)
    except (ProcessLookupError, PermissionError):
        return True
    return False


def reap_group(child: subprocess.Popen, grace: float = 5.0) -> list[int]:
    """Kill `child`'s process group; return the live pids left in it (empty = clean).

    What: SIGTERM, then SIGKILL, to the group. After each signal it waits for
    the leader, so a reaped leader is never a zombie counted as leaked, then
    polls the group until it is gone. `ps` has the last word, skipping zombies.
    """
    pgid = child.pid
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pgid, sig)
        except (ProcessLookupError, PermissionError):
            break
        deadline = time.monotonic() + grace
        try:
            child.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            continue
        while not group_gone(pgid) and time.monotonic() < deadline:
            time.sleep(0.2)
    out = subprocess.run(["ps", "-axo", "pid=,pgid=,stat="], capture_output=True, text=True).stdout
    rows = (line.split() for line in out.splitlines() if line.strip())
    return [int(r[0]) for r in rows if len(r) >= 3 and int(r[1]) == pgid and not r[2].startswith("Z")]


def project_slug(path: Path) -> str:
    """Claude Code's transcript directory name for a cwd."""
    return re.sub(r"[^A-Za-z0-9]", "-", str(path.resolve()))


def one_run(lever: str, args, env: dict, root: Path, prompt_file: Path, mcp_file: Path,
            names: tuple[list[str], list[str]], index: int) -> dict:
    """Launch one throwaway PM for `lever` in `root/project`; return its numbers or the error."""
    parts = LEVERS[lever][2]
    skip = not_applicable(lever, args.project_info)
    if skip:  # #8998 review: a lever with no target is never a measured 0.
        return {"lever": lever, "run": index, "error": skip, "leaked_pids": []}
    settings_file = None
    if parts:
        settings_file = root / f"settings-{lever}.json"
        settings_file.write_text(json.dumps(settings_body(parts, *names, project=args.project_info), indent=1))
    catchup = LEVERS[lever][3]
    prompt = (CATCHUP_PROMPT.format(tool=CATCHUP_TOOL, project=args.catchup_project)
              if catchup else FIRST_TURN_PROMPT)
    argv = build_argv(lever, prompt_file, mcp_file, settings_file, args.model, prompt,
                      6 if catchup else 1)
    stream, errlog = root / f"{lever}-{index}.jsonl", root / f"{lever}-{index}.err"
    record = {"lever": lever, "run": index}
    with open(stream, "w") as out, open(errlog, "w") as err:
        child = subprocess.Popen(argv, cwd=args.project, env=env, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=err, start_new_session=True)
        try:
            child.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            record["error"] = f"timed out after {args.timeout}s"
        finally:
            record["leaked_pids"] = reap_group(child)
    record["exit_code"] = child.returncode
    try:
        with open(stream) as fh:
            record.update(parse_stream(fh))
    except ProbeError as exc:
        record["error"] = f"{record.get('error', '')} {exc}".strip()
    problem = run_problem(record)
    if problem:  # #8998 review: fail closed, quoting the raw cause.
        tail = errlog.read_text(errors="replace")[-800:]
        record["error"] = f"{problem}; exit={child.returncode}; stderr: {tail}"
    return record


def prepare(root: Path, args, env: dict) -> tuple[Path, Path]:
    """The project (throwaway git repo, or --project-dir as given), the tm-composed PM prompt and the builtin MCP file."""
    proj = args.project
    if not args.project_dir:
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


def porcelain(project: Path) -> str:
    """`git status --porcelain` of `project`; raises ProbeError when it is not a git checkout."""
    out = subprocess.run(["git", "-C", str(project), "status", "--porcelain"], capture_output=True, text=True)
    if out.returncode != 0:
        raise ProbeError(f"{project} is not a git checkout: {out.stderr.strip()[:200]}")
    return out.stdout.strip()


def stray_processes(root: Path) -> list[str]:
    """Live processes whose command line names `root` (each run's claude does)."""
    out = subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout
    return [line.strip()[:200] for line in out.splitlines() if str(root) in line]


def cmd_run(args) -> int:
    config_dir = Path(args.config_dir)
    levers = args.levers.split(",")
    unknown = [lv for lv in levers if lv not in LEVERS]
    needs_dir = [lv for lv in levers if lv in PROJECT_LEVERS]
    if needs_dir and not args.project_dir:
        print(f"pm_context_probe: lever(s) {needs_dir} needs --project-dir", file=sys.stderr)
        return 2
    if unknown or not config_dir.is_dir():
        print(f"pm_context_probe: unknown lever(s) {unknown} (known: {list(LEVERS)}) "
              f"or no CLAUDE_CONFIG_DIR at {config_dir}", file=sys.stderr)
        return 2
    root = None
    try:
        env = child_env(dict(os.environ), config_dir)
        root = Path(tempfile.mkdtemp(prefix="pm-context-probe-"))
        check_root(root)
        args.project = Path(args.project_dir).resolve() if args.project_dir else root / "project"
        if args.project_dir:
            check_root(args.project)
            dirty = porcelain(args.project)
            if dirty:
                raise ProbeError(f"--project-dir {args.project} is not clean; the probe needs a "
                                 f"disposable checkout:\n{dirty[:400]}")
    except ProbeError as exc:
        if root:
            shutil.rmtree(root, ignore_errors=True)
        print(f"pm_context_probe: {exc}", file=sys.stderr)
        return 2
    records, failed = [], False
    # #8998 review: catchup reads a throwaway project unless the caller opts in.
    args.catchup_project = args.catchup_project or str(args.project)
    slug_dir = config_dir / "projects" / project_slug(args.project)
    slug_existed = slug_dir.exists()
    try:
        prompt_file, mcp_file = prepare(root, args, env)
        names = tier_names(config_dir)
        args.project_info = project_tier(args.project) if args.project_dir else None
        meta = {"tm": subprocess.run([args.tm, "--version"], capture_output=True, text=True, env=env).stdout.strip(),
                "claude": subprocess.run(["claude", "--version"], capture_output=True, text=True, env=env).stdout.strip(),
                "prompt_bytes": prompt_file.stat().st_size,
                "user_tier_skills": len(names[0]), "user_tier_agents": len(names[1]),
                "date_utc": time.strftime("%Y-%m-%dT%H:%MZ", time.gmtime()),
                "catchup_project": args.catchup_project, "launch": LAUNCH,
                "project_dir": args.project_dir and str(args.project), "project_tier": args.project_info,
                "project_head": args.project_dir and subprocess.run(
                    ["git", "-C", str(args.project), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()}
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
        if not slug_existed:  # never remove transcripts that predate this run
            shutil.rmtree(slug_dir, ignore_errors=True)
        meta_stray = stray_processes(root)
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
    meta["stray_processes"] = meta_stray
    failed |= bool(meta_stray)
    if args.project_dir:
        meta["project_clean_after"] = not porcelain(args.project)
        failed |= not meta["project_clean_after"]
    results = {"meta": meta, "runs": records}
    if args.out:
        Path(args.out).write_text(json.dumps(results, indent=1) + "\n")
    print(render_report(results))
    print(f"stray processes after cleanup: {meta_stray or 'none'}", file=sys.stderr)
    return 1 if failed else 0



def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    run = sub.add_parser("run")
    run.add_argument("--runs", type=int, default=3, help="baseline runs")
    run.add_argument("--lever-runs", type=int, default=1, help="runs per other lever")
    run.add_argument("--levers", default=None,
                     help="comma list (default: every lever; the project levers only with --project-dir)")
    run.add_argument("--project-dir", default=None,
                     help="run in this CLEAN, disposable git checkout instead of a throwaway project")
    run.add_argument("--tm", default="tm", help="tm binary composing the PM prompt")
    run.add_argument("--model", default=None)
    run.add_argument("--config-dir", default=os.environ.get("PM_PROBE_CONFIG_DIR", str(DEFAULT_CONFIG_DIR)))
    run.add_argument("--catchup-project", default=None,
                     help="opt-in real checkout for the catchup lever (default: the throwaway project)")
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
    if args.cmd == "run" and args.levers is None:
        args.levers = ",".join(lv for lv in LEVERS if args.project_dir or lv not in PROJECT_LEVERS)
    # #8998 review: TERM/HUP raise into cmd_run's and one_run's `finally`, which
    # reap the process group and remove the temp tree.
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, raise_interrupted)
    try:
        if args.cmd == "run":
            return cmd_run(args)
        if args.cmd == "parse":
            with open(args.stream) as fh:
                parsed = parse_stream(fh)
            print(json.dumps(parsed))
            problem = run_problem(parsed)
            if problem:
                raise ProbeError(problem)
            return 0
        if args.cmd == "report":
            print(render_report(json.loads(Path(args.results).read_text())))
            return 0
        print("\n".join(sorted(child_env(dict(os.environ), Path(args.config_dir)))))
        return 0
    except ProbeError as exc:
        print(f"pm_context_probe: {exc}", file=sys.stderr)
        return 1
    except Interrupted as exc:
        print(f"pm_context_probe: {exc}; process group reaped, temp tree removed", file=sys.stderr)
        return 128 + exc.signum


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
