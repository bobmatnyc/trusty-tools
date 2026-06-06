#!/usr/bin/env bash
# Regression task runner for open-mpm.
#
# Why: Validates that the harness can execute each regression task end-to-end
# (build → run → produce output files) without LLM regressions silently
# breaking output generation. Each task is a concrete coding challenge; a
# passing run means the prescribed workflow produced at least one output file.
# What: For each task file in .open-mpm/tasks/regression/, runs
# `open-mpm --workflow prescriptive --task-file <file> --out-dir <dir>`
# with a per-task timeout, then checks that the output dir is non-empty.
# Prints PASS/FAIL per task with elapsed time, exits non-zero on any failure.
# Test: `./tests/harness/run_regression.sh --dry-run` (no LLM calls, lists tasks)
#       `./tests/harness/run_regression.sh` (real run, requires OPENROUTER_API_KEY)
#       `./tests/harness/run_regression.sh --task go-simple` (single task)
set -euo pipefail

# ── Argument parsing ──────────────────────────────────────────────────────────

DRY_RUN=false
ONLY_TASK=""
TIMEOUT_SECS=900

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --quick)
      # Run only the bash-simple task with a 300s timeout (finishes in ~140s).
      # Useful for a fast smoke-check before a full regression run.
      TIMEOUT_SECS=300
      ONLY_TASK="bash-simple"
      shift
      ;;
    --task)
      ONLY_TASK="${2:-}"
      shift 2
      ;;
    --timeout)
      TIMEOUT_SECS="${2:-900}"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      echo "Usage: $0 [--dry-run] [--quick] [--task <id>] [--timeout <seconds>]" >&2
      echo "  --quick   Run only bash-simple with a 300s timeout (fast smoke-check)" >&2
      exit 1
      ;;
  esac
done

# ── Paths ─────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TASK_DIR="$PROJECT_ROOT/.open-mpm/tasks/regression"
OUT_BASE="$PROJECT_ROOT/out/regression"
BINARY="$PROJECT_ROOT/target/debug/open-mpm"

# ── Timeout compatibility (macOS lacks GNU coreutils timeout) ─────────────────
# Prefer gtimeout (brew install coreutils), fall back to a perl one-liner shim.
if command -v timeout &>/dev/null; then
  _timeout() { timeout "$@"; }
elif command -v gtimeout &>/dev/null; then
  _timeout() { gtimeout "$@"; }
else
  # perl shim: _timeout <secs> <cmd> [args…]
  # Forks a child, kills it after <secs> seconds, exits 124 on timeout.
  _timeout() {
    local secs="$1"; shift
    perl -e '
      use POSIX ":sys_wait_h";
      my ($secs, @cmd) = @ARGV;
      my $pid = fork; die "fork: $!" unless defined $pid;
      if ($pid == 0) { exec @cmd or die "exec: $!"; }
      local $SIG{ALRM} = sub { kill "TERM", $pid; sleep 2; kill "KILL", $pid; exit 124; };
      alarm $secs;
      waitpid($pid, 0);
      alarm 0;
      my $status = $?;
      exit(($status >> 8) || ($status & 127 ? 128 + ($status & 127) : 0));
    ' -- "$secs" "$@"
  }
fi

# ── Build (skip in dry-run) ───────────────────────────────────────────────────

if ! $DRY_RUN; then
  if [ ! -f "$BINARY" ]; then
    echo "Binary not found — building open-mpm..."
    cargo build --manifest-path="$PROJECT_ROOT/Cargo.toml"
  fi
fi

# ── Collect task files ────────────────────────────────────────────────────────

declare -a TASK_FILES=()
for f in "$TASK_DIR"/*.txt; do
  [[ -f "$f" ]] || continue
  task_id="$(basename "$f" .txt)"
  if [[ -n "$ONLY_TASK" && "$task_id" != "$ONLY_TASK" ]]; then
    continue
  fi
  TASK_FILES+=("$f")
done

if [[ ${#TASK_FILES[@]} -eq 0 ]]; then
  if [[ -n "$ONLY_TASK" ]]; then
    echo "No task found matching: $ONLY_TASK" >&2
    echo "Available tasks:" >&2
    for f in "$TASK_DIR"/*.txt; do
      [[ -f "$f" ]] && echo "  $(basename "$f" .txt)" >&2
    done
    exit 1
  else
    echo "No task files found in $TASK_DIR" >&2
    exit 1
  fi
fi

# ── Dry-run: just list tasks ──────────────────────────────────────────────────

if $DRY_RUN; then
  echo "=== Regression Tasks (dry-run — no LLM calls) ==="
  echo ""
  printf "%-20s  %-60s\n" "TASK ID" "COMMAND"
  printf "%-20s  %-60s\n" "--------------------" "------------------------------------------------------------"
  for f in "${TASK_FILES[@]}"; do
    task_id="$(basename "$f" .txt)"
    out_dir="$OUT_BASE/$task_id"
    printf "%-20s  %s\n" \
      "$task_id" \
      "open-mpm --workflow prescriptive --task-file $f --out-dir $out_dir --timeout $TIMEOUT_SECS"
  done
  echo ""
  echo "Total: ${#TASK_FILES[@]} task(s) would run."
  echo "Remove --dry-run to execute (requires OPENROUTER_API_KEY)."
  exit 0
fi

# ── Live run ──────────────────────────────────────────────────────────────────

PASS=0
FAIL=0
declare -a FAILURES=()

echo "=== Regression Task Runner ==="
echo ""

for f in "${TASK_FILES[@]}"; do
  task_id="$(basename "$f" .txt)"
  out_dir="$OUT_BASE/$task_id"

  mkdir -p "$out_dir"

  printf "[%-20s] running..." "$task_id"
  start_ts=$(date +%s)

  set +e
  _timeout "$TIMEOUT_SECS" "$BINARY" \
    --workflow prescriptive \
    --task-file "$f" \
    --out-dir "$out_dir" \
    > "$out_dir/harness.log" 2>&1
  exit_code=$?
  set -e

  end_ts=$(date +%s)
  elapsed=$(( end_ts - start_ts ))

  if [[ $exit_code -eq 124 ]]; then
    printf " FAIL  (timeout after %ds)\n" "$TIMEOUT_SECS"
    FAIL=$(( FAIL + 1 ))
    FAILURES+=("$task_id: timed out after ${TIMEOUT_SECS}s")
    continue
  fi

  if [[ $exit_code -ne 0 ]]; then
    printf " FAIL  (exit %d, %ds)\n" "$exit_code" "$elapsed"
    FAIL=$(( FAIL + 1 ))
    FAILURES+=("$task_id: exit code $exit_code")
    continue
  fi

  # Check that the output directory contains at least one non-log file.
  file_count=$(find "$out_dir" -maxdepth 3 -type f ! -name "harness.log" | wc -l | tr -d ' ')
  if [[ "$file_count" -eq 0 ]]; then
    printf " FAIL  (no output files produced, %ds)\n" "$elapsed"
    FAIL=$(( FAIL + 1 ))
    FAILURES+=("$task_id: no output files in $out_dir")
    continue
  fi

  printf " PASS  (files=%s, %ds)\n" "$file_count" "$elapsed"
  PASS=$(( PASS + 1 ))
done

echo ""
echo "Results: $PASS passed, $FAIL failed"

if [[ $FAIL -ne 0 ]]; then
  echo ""
  echo "Failures:"
  for f in "${FAILURES[@]}"; do
    echo "  - $f"
  done
  exit 1
fi

exit 0
