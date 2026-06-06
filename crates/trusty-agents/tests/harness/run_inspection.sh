#!/usr/bin/env bash
# Harness inspection test runner (dry-run + live modes).
#
# Why: End-to-end validates the `open-mpm inspect` command produces the
# expected agent routing for each entry in the harness test suite. Dry-run
# (default) exercises registry routing with no LLM cost. Live mode
# (`--live`) additionally runs one PM LLM turn per task and compares the
# PM's delegation decision against the static prediction.
# What: For each hard-coded task, spawns `open-mpm inspect --task "..."`
# (with or without `--dry-run`), parses the JSON output, compares the
# chosen agent against the expected agent name, and prints PASS / FAIL
# with a running tally. Exits non-zero when any task fails.
# Test: `./tests/harness/run_inspection.sh` (dry-run) or
#       `./tests/harness/run_inspection.sh --live` (requires API key).
set -euo pipefail

LIVE_MODE=false
if [[ "${1:-}" == "--live" ]]; then
  LIVE_MODE=true
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BINARY="$PROJECT_ROOT/target/debug/open-mpm"

if [ ! -f "$BINARY" ]; then
  echo "Building open-mpm..."
  cargo build --manifest-path="$PROJECT_ROOT/Cargo.toml"
fi

PASS=0
FAIL=0
FAILURES=()

run_inspection() {
  local id="$1"
  local task="$2"
  local expected_agents="$3"  # comma-separated list of acceptable agents

  printf "[%-22s] " "$id"

  if $LIVE_MODE; then
    result=$("$BINARY" inspect --task "$task" 2>/dev/null || echo "{}")
    actual_agent=$(echo "$result" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    v = (d.get('live_decision') or {}).get('agent')
    print(v if v is not None else 'NONE')
except Exception:
    print('ERROR')
" 2>/dev/null || echo "ERROR")
    matches=$(echo "$result" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    v = (d.get('validation') or {}).get('live_matches_static')
    print(v)
except Exception:
    print('ERROR')
" 2>/dev/null || echo "ERROR")
  else
    result=$("$BINARY" inspect --task "$task" --dry-run 2>/dev/null || echo "{}")
    actual_agent=$(echo "$result" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    v = d.get('registry', {}).get('best_match')
    print(v if v is not None else 'NONE')
except Exception:
    print('ERROR')
" 2>/dev/null || echo "ERROR")
    matches="N/A"
  fi

  # Accept any of the comma-separated expected agents
  local matched=false
  IFS=',' read -ra acceptable <<< "$expected_agents"
  for acceptable_agent in "${acceptable[@]}"; do
    if [ "$actual_agent" = "$acceptable_agent" ]; then
      matched=true
      break
    fi
  done

  if $matched; then
    echo "PASS  agent=$actual_agent  static_match=$matches"
    PASS=$((PASS + 1))
  else
    echo "FAIL  expected=$expected_agents  got=$actual_agent  static_match=$matches"
    FAIL=$((FAIL + 1))
    FAILURES+=("$id: expected=$expected_agents got=$actual_agent")
  fi
}

if $LIVE_MODE; then
  echo "=== Harness Inspection Tests (live — real LLM calls) ==="
else
  echo "=== Harness Inspection Tests (dry-run) ==="
fi
run_inspection "python-csv"        "Write a Python script that reads a CSV file and outputs JSON"                                         "python-engineer"
run_inspection "fastapi-crud"      "Create a FastAPI REST API with CRUD endpoints and pytest tests"                                       "plan-agent,python-engineer"
run_inspection "bash-backup"       "Write a bash script that backs up a directory to a tar.gz archive"                                    "local-ops-agent"
run_inspection "website-check"     "Check if https://httpbin.org/get is accessible and report the HTTP status"                            "research-agent,local-ops-agent"
run_inspection "research-rust"     "Research the current best practices for async Rust in 2024 — compare tokio vs async-std"             "research-agent"
run_inspection "api-docs"          "Write API documentation for a REST API with user management endpoints"                                "docs-agent,documentation"
run_inspection "plan-weather"      "Plan a multi-file Python project: a CLI weather app with API client and tests"                        "plan-agent"
run_inspection "qa-pytest"         "Run the pytest suite in ./out/weather-app/ and report failing tests"                                  "qa-agent"
run_inspection "docker-setup"      "Write a Dockerfile and docker-compose.yml for a FastAPI application"                                  "local-ops-agent,python-engineer"
run_inspection "readme-update"     "Write and save updated README.md content documenting the new CLI commands"                            "docs-agent"
run_inspection "go-simple"         "Write a Go HTTP client CLI with exponential backoff retry and httptest tests"                          "engineer"
run_inspection "bash-simple"       "Write a bash log analyzer script with --since date filter and summary table"                          "local-ops-agent"
run_inspection "py-async"          "Write a Python async HTTP fetcher using asyncio and aiohttp with Semaphore concurrency limit"          "python-engineer"
run_inspection "ts-zod"            "Create TypeScript Zod schemas for UserProfile, ApiResponse, and PaginatedList with strict types"       "engineer"
run_inspection "rs-tokio"          "Write a Rust async task queue with tokio mpsc, semaphore concurrency, and graceful shutdown"           "engineer"

echo ""
echo "Results: $PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
  echo ""
  echo "Failures:"
  for f in "${FAILURES[@]}"; do
    echo "  - $f"
  done
  exit 1
fi
exit 0
