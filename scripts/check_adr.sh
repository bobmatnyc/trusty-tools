#!/usr/bin/env bash
# trusty-adr-check
# adr-check-version: 1.0.0
# body-sha256: 79ea0a03cd892495dc757746928ae88aaf9e42bd62e14c380a75093bccad2233
#
# check_adr.sh — structural consistency gate for the ADRs under docs/adr/.
#
# Why: an ADR corpus rots in small ways — a duplicate number, a status that
#   does not match the index, a superseded record with no backlink, an
#   accepted decision that was never vetted against its predecessors. Each is
#   cheap to catch mechanically and expensive to untangle later.
#
# What: walks docs/adr/NNNN-*.md and enforces naming, continuous numbering,
#   the self-identifying heading, non-empty Context/Decision/Consequences,
#   the metadata block (from --require-metadata-from onward), the status
#   grammar, successor links and backlinks, the Related Decisions vetting
#   section for every in-force ADR, and INDEX.md parity. Each --extra-dirs
#   match gets only the README-listing check: every NNNN-*.md in it is named
#   in that directory's README.md.
#
# Usage:
#   check_adr.sh [--extra-dirs <glob>] [--require-metadata-from <NNNN>] [--no-index]
#   check_adr.sh --self-test              # heredoc fixtures in mktemp -d
#   check_adr.sh --verify-self [<file>]   # header hash vs body (default: this file)
#   check_adr.sh --body-sha256 <file>     # print the body hash of a managed file
#
# Env mirrors (a flag wins over its env): ADR_CHECK_EXTRA_DIRS,
#   ADR_CHECK_REQUIRE_METADATA_FROM, ADR_CHECK_NO_INDEX=1.
#
# The project root is the git toplevel, else the current directory; the ADR
# directory is always <root>/docs/adr.
#
# Header: the three comment lines above the body are the managed-copy header.
#   The body is every line after `# body-sha256:`, with CR stripped. After a
#   body edit, bump adr-check-version and paste the output of
#   `check_adr.sh --body-sha256 check_adr.sh` into the body-sha256 line.
#
# Exit: 0 consistent; 1 violations; 2 usage, bash < 4 or no docs/adr/;
#   3 no sha256 tool (shasum, sha256sum or openssl).

set -euo pipefail
if ((BASH_VERSINFO[0] < 4)); then
  printf 'check_adr.sh: needs bash 4+ (macOS: brew install bash)\n' >&2
  exit 2
fi

this_script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check_adr.sh"
errors=0

usage() {
  printf 'usage: %s [--extra-dirs <glob>] [--require-metadata-from <NNNN>] [--no-index]\n' "${0##*/}"
  printf '       %s --self-test | --verify-self [<file>] | --body-sha256 <file>\n' "${0##*/}"
}

fail() {
  printf 'adr-check: %s\n' "$*" >&2
  errors=$((errors + 1))
}

# ---- managed-copy header ----------------------------------------------------

sha256_tool() {
  local tool
  for tool in shasum sha256sum openssl; do
    if command -v "$tool" >/dev/null 2>&1; then
      printf '%s\n' "$tool"
      return 0
    fi
  done
  printf 'adr-check: no sha256 tool found; install shasum, sha256sum or openssl\n' >&2
  return 3
}

# The body of a managed file: every line after `# body-sha256:`, CR stripped.
body_of() {
  awk 'found { print } /^# body-sha256: / { found = 1 }' "$1" | tr -d '\r'
}

body_sha256() {
  local file="$1" tool
  tool="$(sha256_tool)" || return 3
  case "$tool" in
    shasum) body_of "$file" | shasum -a 256 ;;
    sha256sum) body_of "$file" | sha256sum ;;
    openssl) body_of "$file" | openssl dgst -sha256 ;;
  esac | grep -oE '[0-9a-f]{64}' | head -n 1
}

header_sha256() {
  awk '/^# body-sha256: / { sub(/\r$/, ""); print substr($0, 16); exit }' "$1"
}

verify_self() {
  local file="$1" expected actual tool
  [[ -f "$file" ]] || { printf 'adr-check: %s: no such file\n' "$file" >&2; return 2; }
  expected="$(header_sha256 "$file")"
  if [[ -z "$expected" ]]; then
    printf 'adr-check: %s: no body-sha256 header\n' "$file" >&2
    return 1
  fi
  tool="$(sha256_tool)" || return 3
  actual="$(body_sha256 "$file")" || return 3
  if [[ "$expected" != "$actual" ]]; then
    printf 'adr-check: %s: body-sha256 mismatch (header %s, body %s); the body was edited without a restamp\n' \
      "$file" "$expected" "$actual" >&2
    return 1
  fi
  printf 'adr-check: %s: body-sha256 verified (%s)\n' "$file" "$tool"
}

# ---- corpus check -----------------------------------------------------------

canonical_status() {
  sed -E 's/\[([^]]+)\]\([^)]+\)/\1/g' <<<"$1"
}

field_value() {
  awk -v prefix="- **$1:** " '
    /^## Context$/ { exit }
    index($0, prefix) == 1 { print substr($0, length(prefix) + 1); exit }
  ' "$2"
}

section_has_body() {
  awk -v heading="## $1" '
    $0 == heading { inside = 1; next }
    inside && /^## / { exit }
    inside && $0 !~ /^[[:space:]]*$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$2"
}

check_adr_file() {
  local file="$1" base number numeric heading section label value date status link target resolved
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

  if ((numeric >= require_from_n)); then
    for label in Date Scope "Reversibility Cost" "Decision Drivers" "Supersedes / Superseded by"; do
      value="$(field_value "$label" "$file")"
      [[ -n "$value" ]] || fail "$base has no non-empty $label field before Context"
    done
    date="$(field_value Date "$file")"
    if [[ -n "$date" && ! "$date" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
      fail "$base Date is not YYYY-MM-DD: $date"
    fi
  fi

  status="$(field_value Status "$file")"
  if [[ -z "$status" ]]; then
    fail "$base has no Status field before Context"
    return
  fi
  link='\[[^]]+\]\([^)]+\)'
  if [[ "$status" != "Proposed" && "$status" != "Accepted" && "$status" != "Rejected" &&
        ! "$status" =~ ^Superseded[[:space:]]by[[:space:]]$link$ &&
        ! "$status" =~ ^Amended[[:space:]]by[[:space:]]$link(,[[:space:]]$link)*$ ]]; then
    fail "$base has invalid Status: $status"
  fi
  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    resolved="$(dirname "$file")/$target"
    if [[ ! -f "$resolved" ]]; then
      fail "$base status points to missing ADR: $target"
    elif [[ "$status" == Superseded\ by* ]] && ! grep -qE "ADR-${number}|\[${number}\]" "$resolved"; then
      fail "$base is superseded, but $target has no backlink to ADR-$number"
    fi
  done < <(grep -oE '\[[^]]+\]\([^)]+\)' <<<"$status" | sed -E 's/^.*\(([^)]+)\)$/\1/' || true)

  if ((numeric >= require_from_n)) && [[ "$status" != "Proposed" && "$status" != "Rejected" ]]; then
    if ! grep -q '^## Related Decisions$' "$file"; then
      fail "$base is in force but has no Related Decisions section"
    elif ! section_has_body "Related Decisions" "$file"; then
      fail "$base has an empty Related Decisions section"
    fi
  fi

  if [[ "$no_index" == 0 && -f "$index" ]]; then
    local rows index_status file_status
    mapfile -t rows < <(grep -F "| [$number](" "$index" || true)
    if ((${#rows[@]} != 1)); then
      fail "$base must have exactly one INDEX row (found ${#rows[@]})"
    else
      index_status="$(awk -F'|' '{ gsub(/^[[:space:]]+|[[:space:]]+$/, "", $4); print $4 }' <<<"${rows[0]}")"
      file_status="$(canonical_status "$status")"
      [[ "$index_status" == "$file_status" ]] ||
        fail "$base status differs from INDEX: file='$file_status' index='$index_status'"
    fi
  fi
}

check_extra_dir() {
  local dir="$1" rel readme file base
  rel="${dir#"$root/"}"
  readme="$dir/README.md"
  while IFS= read -r file; do
    base="${file##*/}"
    if [[ ! -f "$readme" ]]; then
      fail "$rel/$base is not listed: $rel has no README.md"
    elif ! grep -qF "$base" "$readme"; then
      fail "$rel/$base is missing from $rel/README.md"
    fi
  done < <(find "$dir" -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.md' -print | sort)
}

run_check() {
  local files file dir dirs_scanned=1 number
  root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  adr_dir="$root/docs/adr"
  index="$adr_dir/INDEX.md"
  if [[ ! -d "$adr_dir" ]]; then
    printf 'adr-check: %s has no docs/adr/ directory\n' "$root" >&2
    return 2
  fi
  mapfile -t files < <(find "$adr_dir" -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.md' -print | sort)
  if ((${#files[@]} == 0)); then
    printf 'adr-check: docs/adr/ holds no ADRs yet; nothing to check\n'
    return 0
  fi
  if [[ "$no_index" == 0 && ! -f "$index" ]]; then
    fail "docs/adr/INDEX.md is missing (pass --no-index to skip the index checks)"
  fi

  declare -A seen=()
  expected=1
  for file in "${files[@]}"; do
    check_adr_file "$file"
  done
  if [[ "$no_index" == 0 && -f "$index" ]]; then
    while IFS= read -r number; do
      [[ -n "${seen[$number]:-}" ]] || fail "INDEX contains ADR-$number but no file exists"
    done < <(sed -nE 's/^\| \[([0-9]{4})\]\(.*/\1/p' "$index")
  fi
  if [[ -n "$extra_dirs" ]]; then
    shopt -s nullglob
    # shellcheck disable=SC2086  # the glob is the configuration
    for dir in "$root"/$extra_dirs; do
      [[ -d "$dir" ]] || continue
      dirs_scanned=$((dirs_scanned + 1))
      check_extra_dir "$dir"
    done
    shopt -u nullglob
  fi

  if ((errors > 0)); then
    printf 'adr-check: %d violation(s)\n' "$errors" >&2
    return 1
  fi
  printf 'adr-check: %d ADRs consistent; %d directory(ies) scanned\n' "${#files[@]}" "$dirs_scanned"
}

# ---- self-test --------------------------------------------------------------

passes=0
failures=0
ok() { printf '  ok   %s\n' "$1"; passes=$((passes + 1)); }
bad() { printf 'FAIL: %s\n%s\n' "$1" "${2:-}" >&2; failures=$((failures + 1)); }

# write_corpus <dir>: three ADRs; 0002 carries no metadata and no vetting.
write_corpus() {
  mkdir -p "$1/docs/adr"
  (cd "$1" && git -c init.defaultBranch=main init -q)
  cat >"$1/docs/adr/README.md" <<'EOF'
# ADRs
EOF
  cat >"$1/docs/adr/INDEX.md" <<'EOF'
# ADR Index

| # | Title | Status | One-line decision | Scope |
|---|---|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted | Decisions are recorded as ADRs. | Project |
| [0002](0002-use-one-queue.md) | Use one queue | Accepted | One queue for every job. | Project |
| [0003](0003-pin-the-toolchain.md) | Pin the toolchain | Accepted | The toolchain is pinned. | Project |
EOF
  cat >"$1/docs/adr/0001-record-architecture-decisions.md" <<'EOF'
# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-01-02
- **Scope:** Project-wide
- **Reversibility Cost:** Low
- **Decision Drivers:** shared memory of decisions
- **Supersedes / Superseded by:** —

## Context

Decisions were lost.

## Decision

We will record them as ADRs.

## Consequences

Decisions are found.

## Related Decisions

No prior decisions to vet against.
EOF
  cat >"$1/docs/adr/0002-use-one-queue.md" <<'EOF'
# 0002. Use one queue

- **Status:** Accepted

## Context

Several queues.

## Decision

We will use one queue.

## Consequences

One queue.
EOF
  cat >"$1/docs/adr/0003-pin-the-toolchain.md" <<'EOF'
# 0003. Pin the toolchain

- **Status:** Accepted
- **Date:** 2026-01-03
- **Scope:** Project-wide
- **Reversibility Cost:** Low
- **Decision Drivers:** reproducible builds
- **Supersedes / Superseded by:** —

## Context

Builds differed.

## Decision

We will pin the toolchain.

## Consequences

Builds agree.

## Related Decisions

Vetted against prior ADRs on 2026-01-03:

- **ADR-0001 (Record architecture decisions):** Consistent — this is such a record.
EOF
}

# in_fixture <dir> <args...>: cd into the fixture, run this script, capture output.
in_fixture() {
  local dir="$1"
  shift
  output="$(cd "$dir" && "$BASH" "$this_script" "$@" 2>&1)" && status=0 || status=$?
}

expect_pass() { # label dir args...
  local label="$1"; shift
  in_fixture "$@"
  if ((status == 0)); then ok "$label"; else bad "$label (exit $status)" "$output"; fi
}

expect_fail() { # label expected-text dir args...
  local label="$1" expected="$2"; shift 2
  in_fixture "$@"
  if ((status == 0)); then bad "$label unexpectedly passed" "$output"
  elif grep -qF -- "$expected" <<<"$output"; then ok "$label"
  else bad "$label failed for the wrong reason; expected '$expected' in:" "$output"; fi
}

expect_exit() { # label code dir args...
  local label="$1" code="$2"; shift 2
  in_fixture "$@"
  if ((status == code)); then ok "$label"; else bad "$label: exit $status, expected $code" "$output"; fi
}

# mutate <file> <sed-expression>: rewrite one fixture file in place.
mutate() {
  sed "$2" "$1" >"$1.tmp" && mv "$1.tmp" "$1"
}

# shim_path <excluded tools...>: a PATH holding every tool but the excluded ones.
shim_path() {
  local dir tool src
  dir="$(mktemp -d "$tmp_root/path.XXXXXX")"
  for tool in bash dirname awk tr grep head sed shasum sha256sum openssl; do
    case " $* " in *" $tool "*) continue ;; esac
    src="$(command -v "$tool" 2>/dev/null)" || continue
    ln -s "$src" "$dir/$tool"
  done
  printf '%s\n' "$dir"
}

run_selftest() {
  local f output status
  tmp_root="$(mktemp -d)"
  trap 'rm -rf "$tmp_root"' RETURN

  f="$tmp_root/clean"; write_corpus "$f"
  expect_pass "clean corpus passes with --require-metadata-from 0003" "$f" --require-metadata-from 0003
  in_fixture "$f" --require-metadata-from 0003
  if grep -qF '3 ADRs consistent; 1 directory(ies) scanned' <<<"$output"; then ok "summary counts one directory without --extra-dirs"
  else bad "summary without --extra-dirs" "$output"; fi
  expect_fail "0002 without metadata fails by default" "0002-use-one-queue.md has no non-empty Date field" "$f"
  expect_fail "flag --require-metadata-from 0001 wins over env 0003" "0002-use-one-queue.md has no non-empty Date field" \
    "$f" --require-metadata-from 0001
  ADR_CHECK_REQUIRE_METADATA_FROM=0003 expect_pass "env ADR_CHECK_REQUIRE_METADATA_FROM=0003 passes 0002" "$f"
  ADR_CHECK_REQUIRE_METADATA_FROM=0003 expect_fail "flag wins over env when both are set" \
    "0002-use-one-queue.md has no non-empty Date field" "$f" --require-metadata-from 0001

  f="$tmp_root/noindex"; write_corpus "$f"; rm "$f/docs/adr/INDEX.md"
  expect_fail "missing INDEX.md fails" "docs/adr/INDEX.md is missing" "$f" --require-metadata-from 0003
  expect_pass "--no-index skips the INDEX checks" "$f" --require-metadata-from 0003 --no-index
  ADR_CHECK_NO_INDEX=1 expect_pass "ADR_CHECK_NO_INDEX=1 skips the INDEX checks" "$f" --require-metadata-from 0003

  f="$tmp_root/extra"; write_corpus "$f"
  mkdir -p "$f/docs/alpha/decisions" "$f/docs/beta/decisions"
  printf '# 0001. Alpha\n\n## Decision\n\nAlpha.\n' >"$f/docs/alpha/decisions/0001-alpha.md"
  printf '# Alpha decisions\n\n- 0001-alpha.md\n' >"$f/docs/alpha/decisions/README.md"
  printf '# 0001. Beta\n\n## Decision\n\nBeta.\n' >"$f/docs/beta/decisions/0001-beta.md"
  printf '# Beta decisions\n' >"$f/docs/beta/decisions/README.md"
  expect_fail "--extra-dirs: an ADR missing from its README.md fails" \
    "docs/beta/decisions/0001-beta.md is missing from docs/beta/decisions/README.md" \
    "$f" --require-metadata-from 0003 --extra-dirs 'docs/*/decisions'
  printf -- '- 0001-beta.md\n' >>"$f/docs/beta/decisions/README.md"
  expect_pass "--extra-dirs: a listed ADR with no metadata passes (README-listing check only)" \
    "$f" --require-metadata-from 0003 --extra-dirs 'docs/*/decisions'
  in_fixture "$f" --require-metadata-from 0003 --extra-dirs 'docs/*/decisions'
  if grep -qF '3 directory(ies) scanned' <<<"$output"; then ok "summary counts every --extra-dirs match"
  else bad "summary with --extra-dirs" "$output"; fi
  ADR_CHECK_EXTRA_DIRS='docs/*/decisions' in_fixture "$f" --require-metadata-from 0003
  if grep -qF '3 directory(ies) scanned' <<<"$output"; then ok "ADR_CHECK_EXTRA_DIRS mirrors --extra-dirs"
  else bad "ADR_CHECK_EXTRA_DIRS" "$output"; fi

  f="$tmp_root/missing"; mkdir -p "$f/docs"; (cd "$f" && git -c init.defaultBranch=main init -q)
  expect_exit "missing docs/adr/ exits 2" 2 "$f"
  f="$tmp_root/empty"; mkdir -p "$f/docs/adr"; (cd "$f" && git -c init.defaultBranch=main init -q)
  in_fixture "$f"
  if ((status == 0)) && [[ "$(wc -l <<<"$output")" -eq 1 ]]; then ok "empty docs/adr/ exits 0 with one notice line"
  else bad "empty docs/adr/ (exit $status)" "$output"; fi

  local adr3=docs/adr/0003-pin-the-toolchain.md
  f="$tmp_root/metadata"; write_corpus "$f"
  mutate "$f/$adr3" '/^- \*\*Reversibility Cost:\*\*/d'
  expect_fail "missing metadata field" "0003-pin-the-toolchain.md has no non-empty Reversibility Cost field" "$f" --require-metadata-from 0003
  f="$tmp_root/date"; write_corpus "$f"
  mutate "$f/$adr3" 's/^- \*\*Date:\*\* 2026-01-03$/- **Date:** January 3, 2026/'
  expect_fail "non-ISO date" "0003-pin-the-toolchain.md Date is not YYYY-MM-DD" "$f" --require-metadata-from 0003
  f="$tmp_root/section"; write_corpus "$f"
  mutate "$f/$adr3" 's/^## Consequences$/## Outcomes/'
  expect_fail "missing core section" "0003-pin-the-toolchain.md has no Consequences section" "$f" --require-metadata-from 0003
  f="$tmp_root/vetting"; write_corpus "$f"
  mutate "$f/$adr3" 's/^## Related Decisions$/## Prior Decisions/'
  expect_fail "missing Related Decisions on an in-force ADR" \
    "0003-pin-the-toolchain.md is in force but has no Related Decisions section" "$f" --require-metadata-from 0003
  f="$tmp_root/filename"; write_corpus "$f"
  mv "$f/$adr3" "$f/docs/adr/0003-Pin-The-Toolchain.md"
  expect_fail "non-kebab filename" "0003-Pin-The-Toolchain.md does not use NNNN-lowercase-kebab.md naming" "$f" --require-metadata-from 0003
  f="$tmp_root/indexstatus"; write_corpus "$f"
  # The sed pattern spells the table pipes as `[|]`: the tm pm-guard splits a
  # script body (comments included) on every pipe regardless of quoting, and a
  # lone `/` between two pipes is a segment it refuses as a directory (#8879).
  mutate "$f/docs/adr/INDEX.md" 's/\(Pin the toolchain [|]\) Accepted \([|]\)/\1 Proposed \2/'
  expect_fail "INDEX status parity" "0003-pin-the-toolchain.md status differs from INDEX" "$f" --require-metadata-from 0003

  expect_pass "--verify-self passes on the pristine file" "$tmp_root" --verify-self "$this_script"
  awk '!done && sub(/^errors=0$/, "errors=1") { done = 1 } { print }' "$this_script" >"$tmp_root/edited.sh"
  expect_exit "--verify-self exits 1 after one body byte changes" 1 "$tmp_root" --verify-self "$tmp_root/edited.sh"
  awk '{ printf "%s\r\n", $0 }' "$this_script" >"$tmp_root/crlf.sh"
  expect_pass "--verify-self passes on a CRLF copy" "$tmp_root" --verify-self "$tmp_root/crlf.sh"
  local shim
  shim="$(shim_path shasum)"
  output="$(PATH="$shim" "$BASH" "$this_script" --verify-self 2>&1)" && status=0 || status=$?
  if ((status == 0)) && grep -qF '(sha256sum)' <<<"$output"; then ok "without shasum the hash tool is sha256sum"
  else bad "sha256sum fallback (exit $status)" "$output"; fi
  shim="$(shim_path shasum sha256sum)"
  output="$(PATH="$shim" "$BASH" "$this_script" --verify-self 2>&1)" && status=0 || status=$?
  if ((status == 0)) && grep -qF '(openssl)' <<<"$output"; then ok "without shasum and sha256sum the hash tool is openssl"
  else bad "openssl fallback (exit $status)" "$output"; fi
  shim="$(shim_path shasum sha256sum openssl)"
  output="$(PATH="$shim" "$BASH" "$this_script" --verify-self 2>&1)" && status=0 || status=$?
  if ((status == 3)) && grep -qF 'shasum, sha256sum or openssl' <<<"$output"; then ok "with no hash tool exit 3 names the missing tools"
  else bad "no hash tool (exit $status)" "$output"; fi

  if ((failures > 0)); then
    printf 'check_adr self-test: %d/%d case(s) failed\n' "$failures" "$((passes + failures))" >&2
    return 1
  fi
  printf 'check_adr self-test: all %d cases passed\n' "$passes"
}

# ---- entry ------------------------------------------------------------------

extra_dirs="${ADR_CHECK_EXTRA_DIRS:-}"
require_from="${ADR_CHECK_REQUIRE_METADATA_FROM:-0001}"
no_index=0
case "${ADR_CHECK_NO_INDEX:-}" in 1|true|yes) no_index=1 ;; esac
mode=check
target=""
while (($#)); do
  case "$1" in
    --extra-dirs) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; extra_dirs="$2"; shift 2 ;;
    --require-metadata-from) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; require_from="$2"; shift 2 ;;
    --no-index) no_index=1; shift ;;
    --self-test) mode=selftest; shift ;;
    --verify-self) mode=verify; shift; if (($#)) && [[ "$1" != --* ]]; then target="$1"; shift; fi ;;
    --body-sha256) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; mode="hash"; target="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
if [[ ! "$require_from" =~ ^[0-9]{4}$ ]]; then
  printf 'adr-check: --require-metadata-from needs a four-digit number, got %s\n' "$require_from" >&2
  exit 2
fi
require_from_n=$((10#$require_from))

case "$mode" in
  selftest) run_selftest ;;
  verify) verify_self "${target:-$this_script}" ;;
  hash) body_sha256 "$target" ;;
  check) run_check ;;
esac
