#!/usr/bin/env bash
#
# check_architect_subproject.sh — independence and drift gate for the
# Architect sub-project, python/trusty-architect/ (issue #8436).
#
# Why: #8436 makes the Architect a self-contained sub-project inside the
#   monorepo: no Cargo workspace membership, no imports from the main tree,
#   and no dependency on the private supervisor checkout it was imported from.
#   Its content also ships inside trusty-mpm, and a copy that drifts from the
#   sub-project ships stale text. Acceptance criteria 6 and 7 ask for a check
#   that fails on each of these.
#
# What: exits non-zero, printing one "FAIL <CLASS>: <detail>" line per
#   finding, when any of these holds:
#     CARGO_TOML        a Cargo.toml exists anywhere under the sub-project
#     WORKSPACE_MEMBER  the root Cargo.toml names the sub-project or python/
#     MAIN_TREE_IMPORT  a sub-project file imports a trusty_* Python module,
#                       cites a crates/ path, or climbs out with ../..
#     OPERATOR_PATH     a scanned file names a home-directory path
#                       (/Users/, /home/, or Claude Code's -Users- slug form)
#                       or a <projects>/<owner>/supervisor checkout path
#     SECRET            a scanned file carries a GitHub or API token shape
#     OWNER_NAME        a scanned file names the owner (whole-word handles)
#     ORG               a scanned file cites a GitHub org outside the allow-list
#     HOSTNAME          a scanned file names a .local/.lan/.internal/.ts.net host
#     SLACK_ID          a scanned file carries a Slack id shape
#     EMAIL             a scanned file carries an email outside example domains
#     PATTERNS          the pattern file or deny file is missing or malformed
#     SCAN_ROOT         the content or assets scan root does not exist
#     GREP_ERROR        grep failed (exit 2+); "no match" alone stays exit 0
#     DRIFT             a shipped copy is missing or differs from its source
#     UNPAIRED          a file under crates/trusty-mpm/src/assets/architect/
#                       is no pair's shipped copy, so no source guards it
#     EMPTY             the sub-project holds no files (a vacuous pass)
#   Runtime state (inbox/, __pycache__/, .pytest_cache/) is not scanned.
#   The leak classes (#9996) come from scripts/architect_leak_patterns.txt,
#   the table `check_content.py leaks` also reads, and run over three roots:
#   python/trusty-architect, content/instructions/architect and
#   crates/trusty-mpm/src/assets/architect. ARCHITECT_LEAK_PATTERNS points at
#   another pattern file; ARCHITECT_LEAK_DENY_FILE names an optional
#   out-of-tree file of private names (unset = skipped; CI leaves it unset).
#
# Usage: scripts/check_architect_subproject.sh [--root <repo-root>] [--list-pairs]
#   --root points the gate at another tree; the selftest uses it.
#   --list-pairs prints the shipped-copy pairs, one "<source>|<copy>" per
#   line, and exits 0; the selftest plants a drift in each copy it names.
#
# Test: scripts/check_architect_subproject_selftest.sh plants each violation
#   class in a temporary tree and asserts the gate fails on it.
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIST_PAIRS=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root)
      [ -n "${2:-}" ] || { echo "usage: $0 [--root <repo-root>] [--list-pairs]" >&2; exit 2; }
      ROOT="$(cd "$2" && pwd)"
      shift 2 ;;
    --list-pairs) LIST_PAIRS=1; shift ;;
    *) echo "usage: $0 [--root <repo-root>] [--list-pairs]" >&2; exit 2 ;;
  esac
done

SUB_REL="python/trusty-architect"
SUB="$ROOT/$SUB_REL"
CONTENT_REL="content/instructions/architect"
ASSETS_REL="crates/trusty-mpm/src/assets/architect"
PATTERNS_FILE="${ARCHITECT_LEAK_PATTERNS:-$(dirname "${BASH_SOURCE[0]}")/architect_leak_patterns.txt}"
DENY_FILE="${ARCHITECT_LEAK_DENY_FILE:-}"
TAB="$(printf '\t')"

# Shipped copies: "<path under the sub-project>|<path under the repo root>",
# one per line. P4 added what `tm fleet init` writes into the Architect.
PAIRS="
skills/tm-architect-setup.md|content/skills/tm-architect-setup.md
skills/tm-fleet-check.md|content/instructions/architect/skills/tm-fleet-check.md
skills/tm-context-refresh.md|content/instructions/architect/skills/tm-context-refresh.md
scripts/fleet-poll.py|crates/trusty-mpm/src/assets/architect/scripts/fleet-poll.py
scripts/fleet-classify.py|crates/trusty-mpm/src/assets/architect/scripts/fleet-classify.py
scripts/input-state.py|crates/trusty-mpm/src/assets/architect/scripts/input-state.py
scripts/quiet-sessions.py|crates/trusty-mpm/src/assets/architect/scripts/quiet-sessions.py
scripts/self-ctx.py|crates/trusty-mpm/src/assets/architect/scripts/self-ctx.py
scripts/start-fleet-poll.sh|crates/trusty-mpm/src/assets/architect/scripts/start-fleet-poll.sh
templates/CLAUDE.md|content/instructions/architect/templates/CLAUDE.md
templates/gitignore|crates/trusty-mpm/src/assets/architect/templates/gitignore
templates/records/state.md|crates/trusty-mpm/src/assets/architect/templates/records/state.md
templates/records/actions.md|crates/trusty-mpm/src/assets/architect/templates/records/actions.md
"
if [ "$LIST_PAIRS" -eq 1 ]; then
  for pair in $PAIRS; do echo "$pair"; done
  exit 0
fi

fail=0
report() { # $1 = class, $2 = detail
  echo "FAIL $1: $2"
  fail=1
}

if [ ! -d "$SUB" ]; then
  report EMPTY "$SUB_REL does not exist"
  exit 1
fi

ERRF="$(mktemp "${TMPDIR:-/tmp}/architect-gate.XXXXXX")"
trap 'rm -f "$ERRF"' EXIT

# Scan roots that exist; a missing content or assets root is a SCAN_ROOT
# failure, never a silent skip (#9996).
ROOTS=("$SUB")
for rel in "$CONTENT_REL" "$ASSETS_REL"; do
  if [ -d "$ROOT/$rel" ]; then
    ROOTS+=("$ROOT/$rel")
  else
    report SCAN_ROOT "$rel does not exist, so it was not scanned"
  fi
done

scan_rule() { # $1 class, $2 flags (i|-), $3 ERE, $4 allow ERE or "", $5.. roots
  local class="$1" flags="$2" match="$3" allow="$4" rc out line f rest n m opt=-rInoE a=-xE
  shift 4
  if [ "$flags" = i ]; then opt=-rInoEi; a=-xEi; fi
  rc=0
  out="$(grep "$opt" --exclude-dir=inbox --exclude-dir=__pycache__ \
    --exclude-dir=.pytest_cache -e "$match" "$@" 2>"$ERRF")" || rc=$?
  if [ "$rc" -ge 2 ]; then
    report GREP_ERROR "$class: grep exited $rc: $(head -n 1 "$ERRF")"
    return 0
  fi
  if [ "$rc" -eq 1 ]; then return 0; fi
  while IFS= read -r line; do
    f="${line%%:*}"; rest="${line#*:}"; n="${rest%%:*}"; m="${rest#*:}"
    if [ -n "$allow" ]; then
      rc=0
      printf '%s\n' "$m" | grep "$a" -e "$allow" >/dev/null 2>"$ERRF" || rc=$?
      if [ "$rc" -eq 0 ]; then continue; fi
      if [ "$rc" -ge 2 ]; then
        report GREP_ERROR "$class: allow pattern failed: $(head -n 1 "$ERRF")"
        return 0
      fi
    fi
    # A secret is named by place only: the gate must not echo it into a log.
    if [ "$class" = SECRET ]; then m="(redacted)"; fi
    report "$class" "${f#"$ROOT"/}:$n: $m"
  done <<EOF
$out
EOF
}

scan() { # $1 = class, $2 = extended regex; the sub-project only
  scan_rule "$1" - "$2" "" "$SUB"
}

load_rules() { # appends "class<TAB>flags<TAB>match<TAB>allow" lines to RULES; fails closed
  local file="$1" line num=0 class flags rest match allow
  if [ ! -f "$file" ]; then
    report PATTERNS "pattern file missing: $file"
    return 0
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    num=$((num + 1))
    line="${line%$'\r'}"
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in
      *"$TAB"*"$TAB"*) ;;
      *) report PATTERNS "$file:$num: expected CLASS<TAB>FLAGS<TAB>MATCH[<TAB>ALLOW]"; continue ;;
    esac
    class="${line%%"$TAB"*}"; rest="${line#*"$TAB"}"
    flags="${rest%%"$TAB"*}"; rest="${rest#*"$TAB"}"
    case "$rest" in
      *"$TAB"*) match="${rest%%"$TAB"*}"; allow="${rest#*"$TAB"}" ;;
      *) match="$rest"; allow="" ;;
    esac
    if [ -z "$class" ]; then report PATTERNS "$file:$num: empty class"; continue; fi
    if [ -z "$match" ]; then report PATTERNS "$file:$num: empty pattern for $class"; continue; fi
    case "$flags" in i|-) ;; *) report PATTERNS "$file:$num: flags must be i or -"; continue ;; esac
    RULES="$RULES$class$TAB$flags$TAB$match$TAB$allow
"
  done <"$file"
  [ -n "$RULES" ] || report PATTERNS "$file holds no patterns"
}

# The optional out-of-tree deny file: one private name per line, whole words.
load_deny() { # $1 = file
  local file="$1" line alt="" num=0
  if [ ! -f "$file" ]; then
    report PATTERNS "deny file missing: $file"
    return 0
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    num=$((num + 1))
    line="${line%$'\r'}"
    case "$line" in ''|'#'*) continue ;; esac
    if ! printf '%s' "$line" | grep -qE '^[A-Za-z0-9][A-Za-z0-9 ._-]*$'; then
      report PATTERNS "$file:$num: a name holds only letters, digits, space . _ -"
      continue
    fi
    line="${line//./\\.}"
    alt="${alt:+$alt|}$line"
  done <"$file"
  if [ -z "$alt" ]; then
    report PATTERNS "$file holds no names"
    return 0
  fi
  RULES="$RULES""OWNER_NAME${TAB}i${TAB}(^|[^A-Za-z0-9_])($alt)([^A-Za-z0-9_]|\$)$TAB
"
}

RULES=""
load_rules "$PATTERNS_FILE"
if [ -n "$DENY_FILE" ]; then load_deny "$DENY_FILE"; fi
if [ "$fail" -ne 0 ]; then
  # Without the patterns the scan would pass vacuously: stop here.
  echo "check_architect_subproject: FAILED (cannot scan; fix the lines above)" >&2
  exit 1
fi

files="$(find "$SUB" -type f ! -path '*/inbox/*' ! -path '*/__pycache__/*' \
  ! -path '*/.pytest_cache/*' | wc -l | tr -d ' ')"
[ "$files" -gt 0 ] || report EMPTY "$SUB_REL holds no files"

cargo="$(find "$SUB" -name Cargo.toml -type f)"
if [ -n "$cargo" ]; then
  while IFS= read -r f; do report CARGO_TOML "${f#"$ROOT"/}"; done <<EOF
$cargo
EOF
fi

# #9822: match the sub-project's path, not the bare name: the Rust crate
# crates/trusty-architect is a workspace member and a dependency by design.
MEMBER_RE='python/trusty-architect|"python(/|")'
if [ -f "$ROOT/Cargo.toml" ] &&
   grep -nE "$MEMBER_RE" "$ROOT/Cargo.toml" >/dev/null; then
  report WORKSPACE_MEMBER "Cargo.toml: $(grep -nE "$MEMBER_RE" \
    "$ROOT/Cargo.toml" | head -1)"
fi

scan MAIN_TREE_IMPORT '^[[:space:]]*(from|import)[[:space:]]+trusty_'
scan MAIN_TREE_IMPORT '(^|[^A-Za-z0-9_.-])crates/'
scan MAIN_TREE_IMPORT '\.\./\.\.'
# The shared table: operator paths, secrets and the #9996 leak classes, over
# every scan root.
while IFS= read -r rule; do
  [ -n "$rule" ] || continue
  c="${rule%%"$TAB"*}"; r="${rule#*"$TAB"}"
  fl="${r%%"$TAB"*}"; r="${r#*"$TAB"}"
  mt="${r%%"$TAB"*}"; al="${r#*"$TAB"}"
  scan_rule "$c" "$fl" "$mt" "$al" "${ROOTS[@]}"
done <<EOF
$RULES
EOF

for pair in $PAIRS; do
  src="$SUB/${pair%%|*}"
  dst="$ROOT/${pair#*|}"
  if [ ! -f "$src" ]; then
    report DRIFT "source missing: $SUB_REL/${pair%%|*}"
  elif [ ! -f "$dst" ]; then
    report DRIFT "shipped copy missing: ${pair#*|}"
  elif ! cmp -s "$src" "$dst"; then
    report DRIFT "${pair#*|} differs from $SUB_REL/${pair%%|*}"
  fi
done

# #8436 P4 fix: a new asset file with no pair would ship unguarded.
ASSET_REL="crates/trusty-mpm/src/assets/architect"
NL="
"
if [ -d "$ROOT/$ASSET_REL" ]; then
  shipped="$(find "$ROOT/$ASSET_REL" -type f ! -name .DS_Store \
    ! -path '*/__pycache__/*' | LC_ALL=C sort)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    rel="${f#"$ROOT"/}"
    case "$PAIRS" in
      *"|$rel$NL"*) ;;
      *) report UNPAIRED "$rel is shipped, but no pair names it; add it to PAIRS" ;;
    esac
  done <<EOF
$shipped
EOF
fi

if [ "$fail" -ne 0 ]; then
  echo "check_architect_subproject: FAILED (fix the lines above)" >&2
  exit 1
fi
echo "check_architect_subproject: OK ($files files scanned, shipped copies match)"
