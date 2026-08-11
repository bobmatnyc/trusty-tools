#!/usr/bin/env bash
# Structural ADR consistency gate for DOC-46.

set -euo pipefail

SCRIPT_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="${ADR_CHECK_REPO_ROOT:-$SCRIPT_REPO_ROOT}"
ADR_DIR="$REPO_ROOT/docs/adr"
INDEX="$ADR_DIR/INDEX.md"
errors=0

shopt -s nullglob

fail() {
  printf 'adr-check: %s\n' "$*" >&2
  errors=$((errors + 1))
}

canonical_status() {
  sed -E 's/\[([^]]+)\]\([^)]+\)/\1/g' <<<"$1"
}

field_value() {
  local label="$1"
  local file="$2"
  awk -v prefix="- **${label}:** " '
    /^## Context$/ { exit }
    index($0, prefix) == 1 { print substr($0, length(prefix) + 1); exit }
  ' "$file"
}

section_has_body() {
  local section="$1"
  local file="$2"
  awk -v heading="## ${section}" '
    $0 == heading { inside=1; next }
    inside && /^## / { exit }
    inside && $0 !~ /^[[:space:]]*$/ { found=1 }
    END { exit(found ? 0 : 1) }
  ' "$file"
}

run_selftest() {
  local tmp_root passes failures
  tmp_root="$(mktemp -d)"
  trap 'rm -rf "$tmp_root"' RETURN
  passes=0
  failures=0

  fixture() {
    local name="$1"
    local root="$tmp_root/$name"
    mkdir -p "$root/docs"
    cp -R "$SCRIPT_REPO_ROOT/docs/adr" "$root/docs/adr"
    printf '%s\n' "$root"
  }

  expect_rejects() {
    local label="$1"
    local root="$2"
    local expected="$3"
    local output
    if output="$(ADR_CHECK_REPO_ROOT="$root" bash "$SCRIPT_REPO_ROOT/scripts/check_adr.sh" 2>&1)"; then
      printf 'FAIL: %s unexpectedly passed\n' "$label" >&2
      failures=$((failures + 1))
    elif grep -qF "$expected" <<<"$output"; then
      printf '  ok   %s\n' "$label"
      passes=$((passes + 1))
    else
      printf 'FAIL: %s rejected for the wrong reason; expected %q in:\n%s\n' \
        "$label" "$expected" "$output" >&2
      failures=$((failures + 1))
    fi
  }

  local baseline metadata date_case section vetting filename
  baseline="$(fixture baseline)"
  if ADR_CHECK_REPO_ROOT="$baseline" bash "$SCRIPT_REPO_ROOT/scripts/check_adr.sh" >/dev/null; then
    printf '  ok   clean corpus passes\n'
    passes=$((passes + 1))
  else
    printf 'FAIL: clean corpus did not pass\n' >&2
    failures=$((failures + 1))
  fi

  metadata="$(fixture metadata)"
  sed '/^- \*\*Reversibility Cost:\*\*/d' \
    "$metadata/docs/adr/0014-native-mcp-support.md" >"$metadata/adr.tmp"
  mv "$metadata/adr.tmp" "$metadata/docs/adr/0014-native-mcp-support.md"
  expect_rejects "missing modern metadata" "$metadata" \
    "0014-native-mcp-support.md has no non-empty Reversibility Cost field"

  date_case="$(fixture date)"
  sed 's/^- \*\*Date:\*\* 2026-07-14$/- **Date:** July 14, 2026/' \
    "$date_case/docs/adr/0014-native-mcp-support.md" >"$date_case/adr.tmp"
  mv "$date_case/adr.tmp" "$date_case/docs/adr/0014-native-mcp-support.md"
  expect_rejects "non-ISO date" "$date_case" \
    "0014-native-mcp-support.md Date is not YYYY-MM-DD"

  section="$(fixture section)"
  sed 's/^## Consequences$/## Outcomes/' \
    "$section/docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md" \
    >"$section/adr.tmp"
  mv "$section/adr.tmp" \
    "$section/docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md"
  expect_rejects "missing core section" "$section" \
    "0040-trusty-mcp-services-absorbs-trusty-gworkspace.md has no Consequences section"

  vetting="$(fixture vetting)"
  sed 's/^## Related Decisions$/## Prior Decisions/' \
    "$vetting/docs/adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md" \
    >"$vetting/adr.tmp"
  mv "$vetting/adr.tmp" \
    "$vetting/docs/adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md"
  expect_rejects "missing accepted-ADR vetting" "$vetting" \
    "0044-main-checkout-write-boundary-and-agent-worktree-ownership.md is in force but has no Related Decisions section"

  filename="$(fixture filename)"
  mv "$filename/docs/adr/0014-native-mcp-support.md" \
    "$filename/docs/adr/0014-Native-MCP-support.md"
  sed 's/0014-native-mcp-support.md/0014-Native-MCP-support.md/' \
    "$filename/docs/adr/INDEX.md" >"$filename/index.tmp"
  mv "$filename/index.tmp" "$filename/docs/adr/INDEX.md"
  expect_rejects "non-kebab filename" "$filename" \
    "0014-Native-MCP-support.md does not use NNNN-lowercase-kebab.md naming"

  if ((failures > 0)); then
    printf 'check_adr self-test: %d/%d mutation case(s) failed\n' \
      "$failures" "$((passes + failures))" >&2
    return 1
  fi

  printf 'check_adr self-test: all %d cases passed\n' "$passes"
}

case "${1:-}" in
  --self-test)
    run_selftest
    exit
    ;;
  "") ;;
  *)
    printf 'usage: %s [--self-test]\n' "${0##*/}" >&2
    exit 2
    ;;
esac

mapfile -t files < <(find "$ADR_DIR" -maxdepth 1 -type f \
  -name '[0-9][0-9][0-9][0-9]-*.md' -print | sort)

if ((${#files[@]} == 0)); then
  fail "no numbered workspace ADRs found"
fi

declare -A seen=()
expected=1

for file in "${files[@]}"; do
  base="${file##*/}"
  number="${base%%-*}"
  numeric=$((10#$number))

  if [[ ! "$base" =~ ^[0-9]{4}-[a-z0-9]+(-[a-z0-9]+)*[.]md$ ]]; then
    fail "$base does not use NNNN-lowercase-kebab.md naming"
  fi

  if [[ -n "${seen[$number]:-}" ]]; then
    fail "duplicate ADR number $number: ${seen[$number]} and $base"
  fi
  seen[$number]="$base"

  if ((numeric != expected)); then
    fail "numbering is not continuous: expected $(printf '%04d' "$expected"), found $number"
    expected=$numeric
  fi
  expected=$((expected + 1))

  heading="$(sed -n '1p' "$file")"
  if [[ ! "$heading" =~ ^#[[:space:]]+${number}[.] ]]; then
    fail "$base heading does not self-identify as $number"
  fi

  for section in Context Decision Consequences; do
    if ! grep -q "^## ${section}$" "$file"; then
      fail "$base has no ${section} section"
    elif ! section_has_body "$section" "$file"; then
      fail "$base has an empty ${section} section"
    fi
  done

  # DOC-46 grandfathered ADR-0001..0013 without structural backfill. Every
  # record created after that baseline must carry the modern metadata block.
  if ((numeric >= 14)); then
    for label in Date Scope "Reversibility Cost" "Decision Drivers" \
      "Supersedes / Superseded by"; do
      value="$(field_value "$label" "$file")"
      if [[ -z "$value" ]]; then
        fail "$base has no non-empty $label field before Context"
      fi
    done

    date="$(field_value Date "$file")"
    if [[ -n "$date" && ! "$date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
      fail "$base Date is not YYYY-MM-DD: $date"
    fi
  fi

  status="$(field_value Status "$file")"
  if [[ -z "$status" ]]; then
    fail "$base has no Status field before Context"
    continue
  fi

  link='\[[^]]+\]\([^)]+\)'
  if [[ "$status" != "Proposed" && "$status" != "Accepted" && "$status" != "Rejected" &&
        ! "$status" =~ ^Superseded[[:space:]]by[[:space:]]$link$ &&
        ! "$status" =~ ^Amended[[:space:]]by[[:space:]]$link(,[[:space:]]$link)*$ ]]; then
    fail "$base has invalid Status: $status"
  fi

  while IFS= read -r target; do
    [[ -z "$target" ]] && continue
    resolved="$(dirname "$file")/$target"
    if [[ ! -f "$resolved" ]]; then
      fail "$base status points to missing ADR: $target"
    elif [[ "$status" == Superseded\ by* ]] &&
         ! grep -qE "ADR-${number}|\[${number}\]" "$resolved"; then
      fail "$base is superseded, but $target has no backlink to ADR-$number"
    fi
  done < <(grep -oE '\[[^]]+\]\([^)]+\)' <<<"$status" | sed -E 's/^.*\(([^)]+)\)$/\1/' || true)

  if ((numeric >= 14)) && [[ "$status" != "Proposed" && "$status" != "Rejected" ]]; then
    if ! grep -q '^## Related Decisions$' "$file"; then
      fail "$base is in force but has no Related Decisions section"
    elif ! section_has_body "Related Decisions" "$file"; then
      fail "$base has an empty Related Decisions section"
    fi
  fi

  mapfile -t rows < <(grep -F "| [$number](" "$INDEX" || true)
  if ((${#rows[@]} != 1)); then
    fail "$base must have exactly one INDEX row (found ${#rows[@]})"
  else
    index_status="$(awk -F'|' '{gsub(/^[[:space:]]+|[[:space:]]+$/, "", $4); print $4}' <<<"${rows[0]}")"
    file_status="$(canonical_status "$status")"
    if [[ "$index_status" != "$file_status" ]]; then
      fail "$base status differs from INDEX: file='$file_status' index='$index_status'"
    fi
  fi
done

while IFS= read -r number; do
  [[ -n "${seen[$number]:-}" ]] || fail "INDEX contains ADR-$number but no file exists"
done < <(sed -nE 's/^\| \[([0-9]{4})\]\(.*/\1/p' "$INDEX")

for readme in "$REPO_ROOT"/docs/*/decisions/README.md; do
  directory="${readme%/README.md}"
  while IFS= read -r file; do
    base="${file##*/}"
    count="$(grep -cF "$base" "$readme" || true)"
    if [[ "$count" == 0 ]]; then
      fail "${file#"$REPO_ROOT/"} is missing from its crate index"
    fi
  done < <(find "$directory" -maxdepth 1 -type f \
    -name '[0-9][0-9][0-9][0-9]-*.md' -print | sort)
done

if ((errors > 0)); then
  printf 'adr-check: %d violation(s)\n' "$errors" >&2
  exit 1
fi

printf 'adr-check: %d workspace ADRs and crate indexes are consistent\n' "${#files[@]}"
