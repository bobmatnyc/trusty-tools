#!/usr/bin/env bash
# Structural ADR consistency gate for DOC-46.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADR_DIR="$REPO_ROOT/docs/adr"
INDEX="$ADR_DIR/INDEX.md"
errors=0

fail() {
  printf 'adr-check: %s\n' "$*" >&2
  errors=$((errors + 1))
}

canonical_status() {
  sed -E 's/\[([^]]+)\]\([^)]+\)/\1/g' <<<"$1"
}

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

  status="$(sed -n '1,20p' "$file" | sed -nE 's/^- \*\*Status:\*\* (.*)$/\1/p' | head -1)"
  if [[ -z "$status" ]]; then
    fail "$base has no Status field in its first 20 lines"
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
    elif ! awk '
      /^## Related Decisions$/ { inside=1; next }
      inside && /^## / { exit }
      inside && /^- / { found=1 }
      END { exit(found ? 0 : 1) }
    ' "$file"; then
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
