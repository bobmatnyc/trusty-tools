#!/usr/bin/env bash
#
# check_worker_dir_excluded.sh — guard for the excluded OKF extraction worker dir.
#
# Why: the worker needs Rust 1.98 (xberg); the workspace is pinned at 1.94
#   (ADR-0029). The worker directory must stay outside the workspace, or the
#   1.94 build breaks (SPEC-OKF-01 §3 (n), §6.1 item 15; #10745).
# What: asserts (1) the root Cargo.toml `[workspace] exclude` lists the worker
#   directory, (2) `cargo metadata` lists no workspace member under it, and
#   (3) when the directory has a Cargo.toml, cargo resolves it as its own root
#   rather than as part of this workspace.
# Usage: scripts/check_worker_dir_excluded.sh [repo-root]
# Exit: 0 when excluded; 1 on any violation.
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
WORKER_DIR="workers/okf-extract"

python3 -I - "$ROOT" "$WORKER_DIR" <<'PY'
import json, subprocess, sys, tomllib
from pathlib import Path

root, worker = Path(sys.argv[1]).resolve(), sys.argv[2]
ws = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
if worker not in ws.get("exclude", []):
    sys.exit(f"FAIL: root Cargo.toml [workspace] exclude does not list {worker}")
print(f"ok: [workspace] exclude lists {worker}")

def metadata(*extra):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", *extra],
        cwd=root, check=True, capture_output=True, text=True).stdout
    return json.loads(out)

members = metadata()["packages"]
bad = [p["name"] for p in members if Path(p["manifest_path"]).is_relative_to(root / worker)]
if bad:
    sys.exit(f"FAIL: cargo metadata lists workspace member(s) under {worker}: {bad}")
print(f"ok: cargo metadata lists {len(members)} members, none under {worker}")

manifest = root / worker / "Cargo.toml"
if manifest.exists():
    m = metadata("--manifest-path", str(manifest))
    if Path(m["workspace_root"]).resolve() == root:
        sys.exit(f"FAIL: {worker}/Cargo.toml resolves into the root workspace")
    print(f"ok: {worker}/Cargo.toml is its own workspace root")
else:
    print(f"note: {worker}/Cargo.toml absent; manifest check skipped")
PY
