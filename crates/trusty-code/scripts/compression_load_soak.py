#!/usr/bin/env python3
"""Load-realistic compression-effectiveness soak (epic #3866 follow-up to
issue #3869 / PR #3887).

Why: PR #3887's soak drove 224 PM turns through `TCODE_MOCK_LLM=echo-soak`
and scored PASS — but that script's own report flagged, as its single
biggest caveat, that it never stressed the 60%-working-context-floor
boundary: five of `SoakEchoLlmClient`'s six per-call turns carry near-empty
arguments (~20-30 bytes) and exactly ONE ~8 KB oversized turn per call; peak
measured overhead was ~7% of a 200K window, nowhere near the 80K-token/40%
cap the epic's guarantee is actually about. This script is the load-realistic
follow-up that report called for: it drives `TCODE_MOCK_LLM=echo-soak-load`
(`crates/trusty-code/src/task/mock_llm_soak_load.rs`), whose every `set_goal`
turn carries a payload sized to approximate a real tool-output magnitude — a
medium `git diff` (~90 KB), a `grep`-style result dump (~130 KB), a `cargo
test` failure log (~160 KB) — chosen so their SUM per call (~95K estimated
tokens) already exceeds the default 80K-token overhead cap on a 200K-window
model, forcing `cadence::enforce_budget`'s continuous per-turn enforcement to
actually do real work on most turns, not fire once across the whole soak.

What: reuses every piece of `compression_soak.py`'s daemon-lifecycle/RPC
plumbing (`spawn_daemon`, `rpc_call`, `wait_for_terminal_status`) unchanged —
only the mock-LLM env var and per-call turn count differ (`echo-soak-load`
scripts the SAME 7-turns/call, resumable-`stop`-ending shape as `echo-soak`,
so `TURNS_PER_CALL` is identical) — then adds a SESSION-FIDELITY check this
epic's task explicitly asked for and the original soak never performed:
after the load-driving calls complete (i.e. after however many compaction
passes fired), (1) `session.get_goals` must report all three goal slots this
run ever touched as EMPTY (every `set_goal` in the script is immediately
followed by a `clear_goal` — if compaction corrupted or silently dropped a
clear, a slot would still show a stale value); (2) `session.get_transcript`
must still return a well-formed transcript (no RPC error, no empty array)
with a message count far below the raw turn count (proof compaction actually
shrank it); (3) one MORE `task.run` call against the SAME session, after all
that compaction pressure, must still complete via `SessionStatus::Finished`
(proof the loop is still healthy post-compaction, not silently wedged or
erroring).

Usage:
  python3 compression_load_soak.py --tcode-bin <path/to/tcode> --calls 40
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from compression_soak import (  # noqa: E402
    build_project_fixture,
    rpc_call,
    spawn_daemon,
    wait_for_terminal_status,
)

# `SoakLoadEchoLlmClient`'s fixed per-call script length
# (crates/trusty-code/src/task/mock_llm_soak_load.rs): identical shape to
# `SoakEchoLlmClient` — 6 tool-call turns + 1 final bare `stop` = 7 real PM
# turns per `task.run` call.
TURNS_PER_CALL = 7

# The three goal slots `SoakLoadEchoLlmClient`'s script ever touches
# (`set_goal(slot, ...)` immediately followed by `clear_goal(slot)` for
# slots 1, 2, 3, every single call) — the fidelity check's expected
# post-soak state.
TOUCHED_SLOTS = (1, 2, 3)

MOCK_LLM_VARIANT = "echo-soak-load"


def run_load_soak(tcode_bin: Path, calls: int, out_dir: Path) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    data_dir = out_dir / "telemetry"
    data_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="tcode-load-soak-project-") as project_dir:
        project = Path(project_dir)
        build_project_fixture(project)

        proc, base_url = spawn_daemon(
            tcode_bin, project, data_dir, mock_llm=MOCK_LLM_VARIANT
        )
        budget_samples = []
        call_wall_times_ms: list[float] = []
        try:
            created = rpc_call(base_url, "session.create", {"task": "load soak turn 0"})
            session_id = created["id"]
            print(f"session_id={session_id}", file=sys.stderr)

            total_turns = 0
            for call_num in range(1, calls + 1):
                t0 = time.monotonic()
                rpc_call(
                    base_url,
                    "task.run",
                    {
                        "task_description": f"load soak call {call_num}",
                        "session_id": session_id,
                        "mode": "daily-driver",
                    },
                    request_id=call_num,
                )
                status = wait_for_terminal_status(base_url, session_id, timeout_s=120)
                call_wall_ms = (time.monotonic() - t0) * 1000
                call_wall_times_ms.append(call_wall_ms)
                total_turns += TURNS_PER_CALL

                budget = rpc_call(
                    base_url, "session.get_context_budget", {"session_id": session_id}
                )
                budget_samples.append(
                    {"call": call_num, "turns": total_turns, "wall_ms": round(call_wall_ms, 1), **budget}
                )
                print(
                    f"call {call_num}/{calls}: status={status} turns~={total_turns} "
                    f"wall_ms={call_wall_ms:.0f} "
                    f"budget={budget.get('working_context_pct')}",
                    file=sys.stderr,
                )
                if status != "finished":
                    print(
                        f"WARNING: call {call_num} ended in status={status}, not finished — "
                        "session may now be permanently terminal",
                        file=sys.stderr,
                    )

            fidelity = run_fidelity_checks(base_url, session_id)

            rpc_call(base_url, "session.cancel", {"session_id": session_id})
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except Exception:
                proc.kill()

    budget_path = out_dir / "context_budget_samples.json"
    budget_path.write_text(json.dumps(budget_samples, indent=2), encoding="utf-8")
    fidelity_path = out_dir / "fidelity_check.json"
    fidelity_path.write_text(json.dumps(fidelity, indent=2), encoding="utf-8")

    return {
        "session_id": session_id,
        "total_turns": total_turns,
        "compression_jsonl": str(data_dir / "compression.jsonl"),
        "context_budget_samples": str(budget_path),
        "fidelity_check": str(fidelity_path),
        "fidelity_pass": fidelity["overall_pass"],
        "mean_call_wall_ms": round(sum(call_wall_times_ms) / len(call_wall_times_ms), 1),
        "max_call_wall_ms": round(max(call_wall_times_ms), 1),
    }


def run_fidelity_checks(base_url: str, session_id: str) -> dict:
    """Session-fidelity checks performed AFTER the load-driving calls (see
    module docs for what each one proves and why).
    """
    result: dict = {}

    goals = rpc_call(base_url, "session.get_goals", {"session_id": session_id})["goals"]
    stale_slots = [g for g in goals if g.get("slot") in TOUCHED_SLOTS]
    result["goals_after_soak"] = goals
    result["goals_clean"] = len(stale_slots) == 0

    # `session.get_transcript` returns a `TranscriptRecord` (session/
    # transcript.rs): `turns` (the surviving, post-compaction turn list —
    # NOT the raw cumulative turn count), plus `compaction_events` (the
    # #2070 threshold/fallback compactor's OWN durable counter — a second,
    # independent corroboration of the JSONL's `tcode-threshold` count).
    transcript = rpc_call(base_url, "session.get_transcript", {"session_id": session_id})
    turns = transcript.get("turns", [])
    result["transcript_turn_count"] = len(turns) if isinstance(turns, list) else None
    result["transcript_readable"] = isinstance(turns, list) and len(turns) > 0
    result["transcript_compaction_events"] = transcript.get("compaction_events")

    resume_status = None
    resume_error = None
    try:
        rpc_call(
            base_url,
            "task.run",
            {
                "task_description": "post-soak fidelity resume call",
                "session_id": session_id,
                "mode": "daily-driver",
            },
            request_id=999_001,
        )
        resume_status = wait_for_terminal_status(base_url, session_id, timeout_s=120)
    except Exception as e:  # noqa: BLE001 — record, don't crash the soak
        resume_error = str(e)
    result["resume_after_soak_status"] = resume_status
    result["resume_after_soak_error"] = resume_error
    result["resume_ok"] = resume_status == "finished"

    result["overall_pass"] = bool(
        result["goals_clean"] and result["transcript_readable"] and result["resume_ok"]
    )
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tcode-bin", type=Path, default=None)
    parser.add_argument(
        "--calls",
        type=int,
        default=40,
        help=f"Number of task.run calls (each drives {TURNS_PER_CALL} PM turns); "
        f"40 * {TURNS_PER_CALL} = {40 * TURNS_PER_CALL} turns",
    )
    parser.add_argument("--out-dir", type=Path, default=None)
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

    out_dir = args.out_dir or Path(tempfile.mkdtemp(prefix="tcode-compression-load-soak-"))

    result = run_load_soak(tcode_bin, args.calls, out_dir)
    print(json.dumps(result, indent=2))
    return 0 if result["fidelity_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
