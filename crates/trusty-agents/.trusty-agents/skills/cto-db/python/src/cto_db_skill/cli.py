"""Subprocess entrypoint invoked by trusty-agents' `python_skill` bridge.

Why: `crates/trusty-agents/src/tools/python_skill.rs` implements a single,
CTO-db-agnostic `PythonSkillToolExecutor` that spawns
`<manifest.python.command> <tool_name>`, writes the tool's JSON arguments to
stdin, and expects one JSON object on stdout. This module is the Python side
of that contract for the cto-db skill specifically — it owns none of the
query logic (that's `db.py`), only argv/stdin/stdout plumbing and exit codes.

What: `main()` reads `sys.argv[1]` as the tool name, parses stdin as JSON
(treating empty stdin as `{}`), calls `db.dispatch`, and prints the result as
a single JSON line to stdout, stamped with the `db_path` it queried and an
`is_fixture` flag (#4860). On any exception — including an unconfigured
database — it prints `{"error": "<message>"}` to stdout and exits 1, never a
raw Python traceback, which would not parse as the JSON the Rust side expects.

Test: `tests/test_cli.py` drives this via `subprocess.run` end-to-end,
including the fixture DB, an unknown tool, an unknown filter value, and the
unconfigured-database refusal.
"""

from __future__ import annotations

import json
import sys

from cto_db_skill import db


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv

    if not argv:
        print(json.dumps({"error": "usage: cto-db-skill <tool_name> (JSON args on stdin)"}))
        return 1

    tool_name = argv[0]

    raw_stdin = sys.stdin.read().strip()
    try:
        args = json.loads(raw_stdin) if raw_stdin else {}
        if not isinstance(args, dict):
            raise ValueError("stdin JSON must be an object")
    except (json.JSONDecodeError, ValueError) as exc:
        print(json.dumps({"error": f"invalid JSON on stdin: {exc}"}))
        return 1

    # #4860: an unconfigured skill refuses here instead of quietly answering
    # from the bundled fixture.
    try:
        db_path = db.resolve_db_path()
    except db.UnconfiguredDatabaseError as exc:
        print(json.dumps({"error": str(exc)}))
        return 1

    try:
        conn = db.open_readonly(db_path)
    except Exception as exc:  # noqa: BLE001 - surfaced to the LLM, never a crash
        print(
            json.dumps(
                {
                    "error": f"failed to open CTO DB at {db_path}: {exc}",
                }
            )
        )
        return 1

    try:
        result = db.dispatch(conn, tool_name, args)
    except Exception as exc:  # noqa: BLE001 - recoverable, mirrors ToolResult::err
        print(json.dumps({"error": str(exc)}))
        return 1
    finally:
        conn.close()

    # #4860: stamp the resolved source so fixture output stays identifiable
    # downstream rather than reading as a real Duetto figure.
    result["db_path"] = str(db_path)
    result["is_fixture"] = db.is_fixture_path(db_path)
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
