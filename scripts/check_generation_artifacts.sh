#!/usr/bin/env bash
#
# check_generation_artifacts.sh — leaked AI-generation / tool-call artifact
# gate (issue #3579).
#
# Why: issue #3499 reported one stray `</content>` fragment leaked into
#   `docs/design/UI/design-system/tokens.css:75`. A repo-wide sweep found
#   FIVE instances across two shapes, all dating to the same commit
#   (`3e25f6bc`) that introduced the files, and all shipped undetected:
#     - three token CSS files with a bare `</content>` mid-file (all at
#       line 75): docs/design/UI/design-system/tokens.css,
#       docs/design/UI/design-system-svelte/src/styles/tokens.css,
#       docs/design/archive/gui-v1/tokens.css
#     - two spec docs ending cold on a raw tool-call fragment:
#       docs/specs/sessions-tui-interactive.md and
#       docs/specs/telui-telegram-ui.md, both `</content>\n</invoke>` at EOF.
#   An audit of every existing gate (check_line_cap.sh, check_test_pointers.sh,
#   check_sld.sh, check_agent_assets.sh, check_buildrs_sync.sh,
#   check_capabilities.sh, check_claude_md_not_tracked.sh,
#   check_instruction_floor.sh, token-drift.yml) confirmed none of them cover
#   this class of defect — token-drift.yml's `:root\s*\{([\s\S]*?)\n\}` regex
#   in particular stops at the first `\n}`, so the CSS artifact sits just
#   outside the parsed block. No markdownlint/remark/vale exists either.
#
# What: scans tracked files for high-signal leaked tool-call/generation
#   artifacts — the literal XML-ish tags emitted by Claude's (and similar
#   agentic tool-callers') tool-invocation syntax: `</content>`, `</invoke>`,
#   `</parameter>`, `<function_calls>`, `</function_calls>`. A match requires
#   the ENTIRE trimmed line to equal one of these tokens exactly — never a
#   substring match, never a match inside backticks or surrounding prose —
#   because this repo legitimately discusses tool-call syntax in agent
#   instruction assets (crates/trusty-mpm/src/assets/), skills, and
#   prompt-engineering specs, and a broad pattern would false-positive on
#   exactly that content within a week of landing.
#
#   Two scopes, chosen by how much legitimate meaning the token can have in
#   that file type (issue #3579's stated priority: tight patterns over broad
#   ones, scoped to file types where the tokens have none):
#
#   SCOPE A — full-file scan, stylesheet languages (*.css, *.scss, *.less):
#     every line is checked. A bare XML-ish tag alone on a line is a hard
#     syntax error in CSS/SCSS/LESS and has never been observed anywhere in
#     this repo's real stylesheets (verified: `git grep` for these tokens
#     across every tracked *.css/*.scss/*.less returns exactly the three
#     known artifacts and nothing else) — so a mid-file hit is unambiguous,
#     which is what catches the tokens.css instances (they sit mid-file, not
#     at EOF).
#
#   SCOPE B — EOF-only scan, prose/doc files (*.md, *.mdx, *.txt, *.rst):
#     only the file's LAST non-blank line is checked, and only when that
#     line is not inside an unterminated fenced code block (``` or ~~~
#     parity from the top of the file — a defensive heuristic: if a doc ever
#     legitimately ends inside an open fenced example, don't guess). Prose
#     files are exactly where legitimate tool-call-syntax discussion lives in
#     this repo, so full-file scanning here would false-positive; but no
#     legitimate document ends cold on a bare, unfenced closing tag with
#     nothing after it — that is precisely the shape of a pasted LLM
#     completion tail, which is what catches the two spec-doc instances.
#
#   EXCLUDED PATHS (generated/vendor/fixture content, not hand-authored):
#     - any path with a `/dist/`, `/ui-dist/`, `/node_modules/`, `/target/`,
#       or `/vendor/` directory segment (built bundles; scanning them is
#       wasted work, not a correctness concern — grep already confirms zero
#       hits in the tracked dist bundles).
#     - crates/trusty-search/tests/benchmark_corpus/synthetic/* — an
#       LLM-authored fixture crate (see its own README: "Not part of the
#       workspace") used as synthetic benchmark corpus text; same exclusion
#       check_test_pointers.sh already carries for the same corpus and the
#       same reason (it imitates realistic content by design; linting it
#       grades fiction, not real drift).
#
# KNOWN, DOCUMENTED TRADEOFF (issue #3579 asks this to be stated plainly, not
#   quietly narrowed): SCOPE B cannot catch a leaked fragment sitting MID-FILE
#   in a .md doc (only EOF) without also risking flags on legitimate
#   discussion of tool-call syntax elsewhere in the document. Given this
#   repo's specs and agent-instruction assets genuinely discuss `<invoke>`/
#   `<parameter>`-shaped syntax in prose, a full-file scan of every .md file
#   was rejected as too broad. EOF-only is the trade made here — it catches
#   both known .md instances (both are EOF-trailing) at the cost of missing a
#   hypothetical mid-file leak in prose. If that gap ever bites, the fix is a
#   documented per-line exclusion around genuine discussion (see the
#   allowlist below), not silently loosening this scope.
#
# ALLOWLIST (ratchet, can only shrink — mirrors .line-cap-allowlist.tsv /
#   .test-pointer-allowlist.tsv): `.generation-artifact-allowlist.tsv`, one
#   `<path>` per row. A listed path has ALL its findings suppressed
#   (coarse — this is a boolean presence check, not a budget, so unlike the
#   SLOC ratchet there is nothing finer to track per file). A listed path
#   with NO current finding is stale and FAILS the gate — remove its row
#   (`--update` does this automatically). Ordinary `--update` never ADDS a
#   row; `--seed` bootstraps by grandfathering every currently-flagged path
#   in one shot (refuses if the allowlist already has rows). Prefer fixing
#   the file over adding a row by hand.
#
#   This gate was seeded once, in the PR that introduced it, against the five
#   known pre-existing instances above (PR #3578, open at the time, fixes
#   them upstream) — see the seeded allowlist's header comment for exactly
#   which five and why. Once PR #3578 merges those rows become stale and this
#   gate will correctly start failing until someone runs `--update` to prune
#   them; that is the intended ratchet behavior, not a bug.
#
# Usage:
#   check_generation_artifacts.sh              # check mode (default)
#   check_generation_artifacts.sh --update      # prune stale allowlist rows
#   check_generation_artifacts.sh --seed        # bootstrap allowlist (refuses
#                                                # if rows already exist)
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX
#   tools only — `git`, `awk`, `sort`. No associative arrays, no bash-4
#   features.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

ALLOWLIST="$REPO_ROOT/.generation-artifact-allowlist.tsv"

MODE="check"
for arg in "$@"; do
  case "$arg" in
    --update) MODE="update" ;;
    --seed) MODE="seed" ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_generation_artifacts: unknown argument: $arg" >&2
      echo "usage: check_generation_artifacts.sh [--update | --seed]" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------------------
# is_excluded_path: repo-relative path prefixes/segments skipped entirely.
# ---------------------------------------------------------------------------
is_excluded_path() {
  case "$1" in
    */dist/*|*/ui-dist/*|*/node_modules/*|*/target/*|*/vendor/*) return 0 ;;
    crates/trusty-search/tests/benchmark_corpus/synthetic/*) return 0 ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------------------
# MARKER_RE: ERE alternation of the exact (trimmed) line content that counts
# as a leaked artifact. Escaping note: `<` `>` `/` need no ERE escaping;
# these are literal, not glob/regex-metachar-bearing tokens.
# ---------------------------------------------------------------------------
MARKER_RE='^(</content>|</invoke>|</parameter>|<function_calls>|</function_calls>)$'

# ---------------------------------------------------------------------------
# scan_css: full-file scan of a stylesheet file. Prints "<line>\t<token>" per
# hit to fd 1.
# ---------------------------------------------------------------------------
scan_css() {
  awk -v re="$MARKER_RE" '
    {
      line = $0
      gsub(/^[ \t]+|[ \t]+$/, "", line)
      if (line ~ re) print NR "\t" line
    }
  ' "$1"
}

# ---------------------------------------------------------------------------
# scan_prose: EOF-only scan of a prose/doc file. Prints "<line>\t<token>" for
# the file's last non-blank line IF it matches MARKER_RE AND is not inside an
# unterminated fenced code block (``` / ~~~ parity from the top of the file).
# Prints nothing when clean. At most one hit per file (there is only one
# "last line").
# ---------------------------------------------------------------------------
scan_prose() {
  awk -v re="$MARKER_RE" '
    {
      raw = $0
      line = raw
      gsub(/^[ \t]+|[ \t]+$/, "", line)
      if (line != "") { last_line = line; last_num = NR }
      if (raw ~ /^[ ]{0,3}(```|~~~)/) fence_count++
    }
    END {
      if (last_num == "") exit 0
      in_fence = (fence_count % 2 == 1)
      if (!in_fence && last_line ~ re) print last_num "\t" last_line
    }
  ' "$1"
}

# ---------------------------------------------------------------------------
# Build the current violation snapshot: "<path>\t<line>\t<token>" for every
# tracked, non-excluded file in scope, written to $1 (truncated first).
# ---------------------------------------------------------------------------
build_snapshot() {
  local out="$1"
  : > "$out"

  git ls-files '*.css' '*.scss' '*.less' | while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    is_excluded_path "$f" && continue
    scan_css "$f" | while IFS=$'\t' read -r line token; do
      [ -n "${line:-}" ] || continue
      printf '%s\t%s\t%s\n' "$f" "$line" "$token" >> "$out"
    done
  done

  git ls-files '*.md' '*.mdx' '*.txt' '*.rst' | while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    is_excluded_path "$f" && continue
    scan_prose "$f" | while IFS=$'\t' read -r line token; do
      [ -n "${line:-}" ] || continue
      printf '%s\t%s\t%s\n' "$f" "$line" "$token" >> "$out"
    done
  done
}

RAW="$(mktemp "${TMPDIR:-/tmp}/genart.raw.XXXXXX")"
trap 'rm -f "$RAW"' EXIT
build_snapshot "$RAW"

ALLOWLIST_READ="$ALLOWLIST"
[ -f "$ALLOWLIST_READ" ] || ALLOWLIST_READ="/dev/null"

# ===========================================================================
# SEED MODE
# ===========================================================================
if [ "$MODE" = "seed" ]; then
  existing_rows=0
  if [ -f "$ALLOWLIST" ]; then
    existing_rows="$(awk '$0 !~ /^#/ && NF>=1' "$ALLOWLIST" | wc -l | tr -d ' ')"
  fi
  if [ "${existing_rows:-0}" -gt 0 ]; then
    echo "check_generation_artifacts --seed: refusing — $ALLOWLIST already has ${existing_rows} row(s). Remove it first (or edit by hand) if you really intend to re-seed." >&2
    exit 1
  fi

  paths="$(awk -F'\t' '{print $1}' "$RAW" | sort -u)"
  count="$(printf '%s\n' "$paths" | awk 'NF' | wc -l | tr -d ' ')"
  {
    echo "# .generation-artifact-allowlist.tsv — grandfathered leaked"
    echo "# AI-generation / tool-call artifacts (issue #3579)."
    echo "# Format: <relative/path>, one per row. A listed path has ALL of its"
    echo "# check_generation_artifacts.sh findings suppressed."
    echo "# Ratchet: rows may only be REMOVED once the file is fixed; the gate"
    echo "# FAILS on any row whose file no longer has a finding (stale — prune"
    echo "# with: scripts/check_generation_artifacts.sh --update)."
    echo "#"
    echo "# Seeded $(date -u +%Y-%m-%d): the five known pre-existing instances"
    echo "# from issue #3499 (docs/design/UI/design-system/tokens.css:75 and its"
    echo "# two sibling token files; docs/specs/sessions-tui-interactive.md and"
    echo "# docs/specs/telui-telegram-ui.md at EOF), all dating to 3e25f6bc and"
    echo "# fixed upstream by PR #3578. New leaks introduced after this point are"
    echo "# NOT grandfathered — fix them for real."
    printf '%s\n' "$paths" | awk 'NF'
  } > "$ALLOWLIST"
  echo "check_generation_artifacts --seed: wrote $ALLOWLIST with ${count} grandfathered path(s)."
  exit 0
fi

# ===========================================================================
# UPDATE MODE — prune stale rows only (never adds).
# ===========================================================================
if [ "$MODE" = "update" ]; then
  if [ ! -f "$ALLOWLIST" ]; then
    echo "check_generation_artifacts --update: no allowlist to prune."
    exit 0
  fi
  current_paths="$(awk -F'\t' '{print $1}' "$RAW" | sort -u)"
  NEW="$(mktemp "${TMPDIR:-/tmp}/genart.new.XXXXXX")"
  trap 'rm -f "$RAW" "$NEW"' EXIT
  pruned=0
  while IFS= read -r rawline; do
    case "$rawline" in
      \#*|"") echo "$rawline" >> "$NEW"; continue ;;
    esac
    p="$(printf '%s' "$rawline" | awk -F'\t' '{print $1}')"
    if printf '%s\n' "$current_paths" | grep -qxF "$p"; then
      echo "$rawline" >> "$NEW"
    else
      pruned=$((pruned + 1))
    fi
  done < "$ALLOWLIST"
  mv "$NEW" "$ALLOWLIST"
  echo "check_generation_artifacts --update: pruned ${pruned} stale row(s) from $ALLOWLIST."
  exit 0
fi

# ===========================================================================
# CHECK MODE
# ===========================================================================
allow_paths="$(awk -F'\t' '$0 !~ /^#/ && NF>=1 {print $1}' "$ALLOWLIST_READ" | sort -u)"

violations=0
seen_allowed=""
while IFS=$'\t' read -r p l t; do
  [ -n "${p:-}" ] || continue
  if printf '%s\n' "$allow_paths" | grep -qxF "$p"; then
    seen_allowed="${seen_allowed}${p}
"
    continue
  fi
  echo "FAIL: ${p}:${l}: leaked generation artifact \`${t}\` — looks like a pasted LLM tool-call fragment, not real content." >&2
  violations=$((violations + 1))
done < "$RAW"

stale=0
if [ -f "$ALLOWLIST" ]; then
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    case "$seen_allowed" in
      *"$p
"*) : ;;
      *)
        echo "FAIL: $p is allowlisted in .generation-artifact-allowlist.tsv but no longer has any finding — remove its row (run --update)." >&2
        stale=$((stale + 1))
        ;;
    esac
  done <<EOF
$allow_paths
EOF
fi

total=$((violations + stale))
if [ "$total" -gt 0 ]; then
  echo "generation-artifacts: ${violations} leaked artifact(s), ${stale} stale allowlist row(s) — FAILED." >&2
  exit 1
fi

echo "generation-artifacts: 0 leaked artifacts — OK."
exit 0
