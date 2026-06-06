#!/usr/bin/env bash
# install.sh — Set up a clean bake-off integration test installation.
#
# Why: Validates the full open-mpm installation experience end-to-end,
#      separate from unit tests that run under `cargo test`.
# What: Builds the release binary, creates a temp project directory, copies
#       fixture CLAUDE.md + .claude/agents/python-engineer.md, stages the
#       bundled .open-mpm/ tree, and drops in a bake-off task file.
# Test: Run `./tests/integration/install.sh`; verify it prints
#       "Installation complete" and that the reported $TEST_DIR contains
#       CLAUDE.md, .claude/agents/python-engineer.md, open-mpm, .open-mpm/, task.txt.

set -euo pipefail

# ─── Locate project root (this script lives at tests/integration/install.sh) ──
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"

# Which bake-off level to stage (default 2 — lightweight smoke test).
LEVEL="${LEVEL:-2}"
TASK_SRC="$PROJECT_ROOT/.open-mpm/tasks/level-${LEVEL}.txt"

echo "=== open-mpm bake-off integration install ==="
echo "Project root: $PROJECT_ROOT"
echo "Fixtures:     $FIXTURES_DIR"
echo "Task level:   $LEVEL"
echo ""

# ─── 1. Build the release binary ──────────────────────────────────────────────
echo "→ Building release binary (cargo build --release)…"
(cd "$PROJECT_ROOT" && cargo build --release --bin open-mpm)

BINARY="$PROJECT_ROOT/target/release/open-mpm"
if [[ ! -x "$BINARY" ]]; then
  echo "❌ Build failed: $BINARY not found or not executable" >&2
  exit 1
fi
echo "  Built: $BINARY"
echo ""

# ─── 2. Create temp test directory ────────────────────────────────────────────
TEST_DIR="$(mktemp -d /tmp/open-mpm-bakeoff-XXXXXX)"
echo "→ Created test directory: $TEST_DIR"

# ─── 3. Stage project files ───────────────────────────────────────────────────
echo "→ Staging CLAUDE.md and .claude/agents/python-engineer.md…"
cp "$FIXTURES_DIR/CLAUDE.md" "$TEST_DIR/CLAUDE.md"
mkdir -p "$TEST_DIR/.claude/agents"
cp "$FIXTURES_DIR/agents/python-engineer.md" "$TEST_DIR/.claude/agents/python-engineer.md"

# .open-mpm/state/ runtime dir (sessions, memory, logs). The bundled config
# tree (agents, skills, workflows, tasks, agent-templates) is copied below.
mkdir -p "$TEST_DIR/.open-mpm/state"

# ─── 4. Stage bake-off task file ──────────────────────────────────────────────
if [[ ! -f "$TASK_SRC" ]]; then
  echo "❌ Task file not found: $TASK_SRC" >&2
  exit 1
fi
cp "$TASK_SRC" "$TEST_DIR/task.txt"
echo "  Staged task: $TASK_SRC → task.txt"

# ─── 5. Copy (or symlink) the binary ──────────────────────────────────────────
echo "→ Installing binary into test dir…"
cp "$BINARY" "$TEST_DIR/open-mpm"
chmod +x "$TEST_DIR/open-mpm"

# ─── 6. Copy bundled harness config ───────────────────────────────────────────
# Includes: .open-mpm/agents, .open-mpm/skills, .open-mpm/workflows,
# .open-mpm/tasks, .open-mpm/agent-templates.
# We copy only the committed-config subdirectories, NOT .open-mpm/state/
# (which is runtime state and was just created empty above).
echo "→ Copying bundled harness .open-mpm/…"
for sub in agents skills workflows tasks agent-templates; do
  if [[ -d "$PROJECT_ROOT/.open-mpm/$sub" ]]; then
    cp -R "$PROJECT_ROOT/.open-mpm/$sub" "$TEST_DIR/.open-mpm/$sub"
  fi
done

# ─── 7. Forward .env.local if present (optional — required for LLM calls) ─────
if [[ -f "$PROJECT_ROOT/.env.local" ]]; then
  cp "$PROJECT_ROOT/.env.local" "$TEST_DIR/.env.local"
  echo "  Forwarded .env.local (API keys present)"
else
  echo "  ⚠️  No .env.local found — set OPENROUTER_API_KEY or ANTHROPIC_API_KEY before running."
fi

echo ""
echo "✅ Installation complete: $TEST_DIR"
echo ""
echo "To run bake-off Level $LEVEL manually:"
echo "    cd $TEST_DIR"
echo "    ./open-mpm --workflow prescriptive --task-file task.txt"
echo ""
echo "To run automated verification:"
echo "    $SCRIPT_DIR/run_bakeoff.sh $TEST_DIR $LEVEL"
echo ""

# Emit path as last line for easy capture (e.g., `TD=$(./install.sh | tail -1)`).
echo "$TEST_DIR"
