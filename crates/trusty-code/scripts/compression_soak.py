#!/usr/bin/env python3
"""Drive a REAL `tcode serve --http` daemon through 200+ PM turns to soak-test
the #2346 cadence compressor, then score the resulting telemetry (issue
#3869, epic #3866 Slice C).

Why: epic #2343's stated success metric ("a 500+ turn interactive session
with `compaction_events == 0` and working context never below 60%") has
never once been evaluated end to end. This harness is that evaluation —
driven over the REAL JSON-RPC wire against a real `tcode` binary (never by
calling into `trusty_code`'s Rust API directly), per the vision spec's
Testability requirement the crate's own e2e suite (`crates/trusty-code/
tests/*_e2e.rs`) already follows.

What: spawns `tcode serve --http --port 0` rooted at a throwaway project
fixture, with `TCODE_MOCK_LLM=echo-soak` (the deterministic, UNBOUNDED
scripted PM client added in `crates/trusty-code/src/task/mock_llm_soak.rs`
for exactly this soak — see that file's docs for why every OTHER mock-LLM
script in this crate is too short-lived) and `TCODE_TELEMETRY_DATA_DIR`
pointed at an isolated output directory (so this synthetic run's numbers
never land in a real `~/.trusty-code/compression.jsonl`). Mints one session
via `session.create`, then calls `task.run(session_id=..., mode="daily-
driver")` repeatedly — each call runs the PM's `cadence: Some(_)`-
instrumented loop for its full `max_turns` (8, `AgentLoopConfig::default`)
turns, since the soak mock LLM always answers with a tool call and never a
bare `stop` — polling `session.status` to completion, then
`session.get_context_budget` as an independent cross-check of the JSONL
telemetry, between calls. Ends with `session.cancel` (cleanup; the session
is normally already terminal by then) and prints the resolved `session_id`
+ output directory so a caller can hand both to `compression_report.py`.

Test: this file IS the test — it's an integration driver against a live
daemon, not unit-testable in the traditional sense (mirrors every
`tests/*_e2e.rs` file's own "the whole file is the test" framing).
Acceptance is the real `compression.jsonl` + report this produces, per issue
#3869's explicit "not unit-testable" note.

Usage:
  python3 compression_soak.py --tcode-bin <path/to/tcode> --calls 32
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

# `SoakEchoLlmClient`'s fixed per-call script length
# (crates/trusty-code/src/task/mock_llm_soak.rs): 6 tool-call turns + 1 final
# bare `stop` = 7 real PM turns per `task.run` call, deliberately kept under
# `AgentLoopConfig::default().max_turns` (8) so every call completes via the
# natural "no tool calls" path (`SessionStatus::Finished`, resumable) rather
# than hitting the turn cap (`SessionStatus::Failed`, permanently terminal).
TURNS_PER_CALL = 7

PM_AGENT_MD = """---
name: pm
model: bedrock/us.anthropic.claude-sonnet-4-6
---

You are the PM. (Soak harness fixture — never actually invoked; the real
turns come from `TCODE_MOCK_LLM=echo-soak`'s scripted client.)
"""


def build_project_fixture(root: Path) -> None:
    """Write the minimal `.claude/agents/pm.md` a daemon needs to boot
    (`tcode serve --project <root>` requires a `.claude/` directory).

    Why: `bedrock/us.anthropic.claude-sonnet-4-6` resolves to a 200K-token
    context window (`provider::routing::resolve_context_window`) — the SAME
    window epic #2343's own worked example (80K overhead cap / 120K working
    floor at 40%) uses, so this soak's percentages are directly comparable
    to the epic's numbers. The model is never actually called: `TCODE_MOCK_
    LLM=echo-soak` swaps in `SoakEchoLlmClient` before any network request
    would happen.
    """
    agents_dir = root / ".claude" / "agents"
    agents_dir.mkdir(parents=True, exist_ok=True)
    (agents_dir / "pm.md").write_text(PM_AGENT_MD, encoding="utf-8")


def spawn_daemon(tcode_bin: Path, project: Path, data_dir: Path) -> tuple[subprocess.Popen, str]:
    """Spawn `tcode serve --http --port 0` and discover its bound address
    from stderr, mirroring `tests/support/mod.rs::spawn_http_daemon_with_env`.
    """
    proc = subprocess.Popen(
        [str(tcode_bin), "serve", "--project", str(project), "--http", "--port", "0"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        env={
            "TCODE_MOCK_LLM": "echo-soak",
            "TCODE_TELEMETRY_DATA_DIR": str(data_dir),
            "PATH": _inherited_path(),
            "HOME": str(Path.home()),
        },
    )
    assert proc.stderr is not None
    base_url = None
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        line = proc.stderr.readline()
        if not line:
            if proc.poll() is not None:
                raise RuntimeError(
                    f"tcode serve exited early (code {proc.returncode}) before reporting its address"
                )
            continue
        match = re.search(r"listening on (http://\S+)", line)
        if match:
            base_url = match.group(1)
            break
    if base_url is None:
        proc.kill()
        raise RuntimeError("timed out waiting for tcode serve to report its bound address")

    # Keep draining stderr for the daemon's life so the pipe never fills and
    # blocks it (mirrors the Rust e2e helper's identical rationale).
    def _drain():
        while proc.poll() is None:
            if proc.stderr.readline() == "":
                break

    threading.Thread(target=_drain, daemon=True).start()
    return proc, base_url


def _inherited_path() -> str:
    import os

    return os.environ.get("PATH", "/usr/bin:/bin")


class RpcError(RuntimeError):
    pass


def rpc_call(base_url: str, method: str, params: dict, request_id: int = 1) -> dict:
    body = json.dumps(
        {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    ).encode("utf-8")
    req = urllib.request.Request(
        f"{base_url}/rpc", data=body, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        payload = json.loads(resp.read())
    if "error" in payload and payload["error"] is not None:
        raise RpcError(f"{method} failed: {payload['error']}")
    return payload["result"]


def wait_for_terminal_status(base_url: str, session_id: str, timeout_s: float = 60) -> str:
    """Poll `session.status` until it leaves `running`/`created`.

    Why: `task.run` returns immediately (the run executes in the
    background) — a second overlapping `task.run` against the same session
    is rejected, so the harness must wait for THIS call's run to reach a
    terminal `SessionStatus` (finished/failed/cancelled/deadline_exceeded,
    per `session::model::SessionStatus::is_terminal`) before issuing the
    next one.
    """
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        status = rpc_call(base_url, "session.status", {"session_id": session_id})["status"]
        if status not in ("running", "created"):
            return status
        time.sleep(0.1)
    raise TimeoutError(f"session {session_id} did not reach a terminal status in {timeout_s}s")


def run_soak(tcode_bin: Path, calls: int, out_dir: Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    data_dir = out_dir / "telemetry"
    data_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="tcode-soak-project-") as project_dir:
        project = Path(project_dir)
        build_project_fixture(project)

        proc, base_url = spawn_daemon(tcode_bin, project, data_dir)
        budget_samples = []
        try:
            created = rpc_call(base_url, "session.create", {"task": "compression soak turn 0"})
            session_id = created["id"]
            print(f"session_id={session_id}", file=sys.stderr)

            total_turns = 0
            for call_num in range(1, calls + 1):
                rpc_call(
                    base_url,
                    "task.run",
                    {
                        "task_description": f"soak call {call_num}",
                        "session_id": session_id,
                        "mode": "daily-driver",
                    },
                    request_id=call_num,
                )
                status = wait_for_terminal_status(base_url, session_id)
                total_turns += TURNS_PER_CALL

                budget = rpc_call(
                    base_url, "session.get_context_budget", {"session_id": session_id}
                )
                budget_samples.append({"call": call_num, "turns": total_turns, **budget})
                print(
                    f"call {call_num}/{calls}: status={status} turns~={total_turns} "
                    f"budget={budget.get('working_context_pct')}",
                    file=sys.stderr,
                )

            rpc_call(base_url, "session.cancel", {"session_id": session_id})
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()

    budget_path = out_dir / "context_budget_samples.json"
    budget_path.write_text(json.dumps(budget_samples, indent=2), encoding="utf-8")

    return {
        "session_id": session_id,
        "total_turns": total_turns,
        "compression_jsonl": str(data_dir / "compression.jsonl"),
        "context_budget_samples": str(budget_path),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tcode-bin",
        type=Path,
        default=None,
        help="Path to the built tcode binary (default: target/debug/tcode "
        "resolved relative to the workspace root)",
    )
    parser.add_argument(
        "--calls",
        type=int,
        default=32,
        help=f"Number of task.run calls (each drives {TURNS_PER_CALL} PM "
        f"turns); 32 * {TURNS_PER_CALL} = {32 * TURNS_PER_CALL} turns, "
        "comfortably past the 200-turn floor",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="Output directory for the isolated telemetry dir + budget "
        "samples (default: a fresh tempdir, printed on stdout)",
    )
    args = parser.parse_args(argv)

    tcode_bin = args.tcode_bin
    if tcode_bin is None:
        workspace_root = Path(__file__).resolve().parents[3]
        tcode_bin = workspace_root / "target" / "debug" / "tcode"
    if not tcode_bin.exists():
        print(
            f"tcode binary not found at {tcode_bin}; build it first with "
            "`cargo build -p trusty-code --bin tcode`",
            file=sys.stderr,
        )
        return 2

    out_dir = args.out_dir or Path(tempfile.mkdtemp(prefix="tcode-compression-soak-"))

    result = run_soak(tcode_bin, args.calls, out_dir)
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
