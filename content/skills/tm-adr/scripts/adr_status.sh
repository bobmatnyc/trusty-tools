#!/usr/bin/env bash
#
# adr_status.sh — report whether each project copy of the tm-adr managed files
# (check_adr.sh, adr-check.yml) matches the deployed skill.
#
# Why: a project keeps copies of check_adr.sh and adr-check.yml while the skill
#   moves on. Status says, per copy, whether an update PR is due, whether the
#   copy was edited locally (so it is never overwritten), or whether tm itself
#   is behind the project. It writes nothing; the update PR is the caller's job.
#
# What: from the git toplevel of the current directory, locates copies with
#   `git grep -l --untracked '^# trusty-adr-check' -- '*.sh' '*.yml'` and adds
#   the fixed paths docs/adr/check_adr.sh and .github/workflows/adr-check.yml
#   when they exist without the marker. Each copy gets one line and one outcome,
#   decided in this order:
#     source            the copy lies under content/skills/tm-adr/ (the skill's
#                       own file); never offered an update PR
#     unmanaged         no marker, no complete header, or no skill file of the
#                       same basename beside this script
#     locally edited    header body-sha256 differs from the body hash, taken
#                       from `check_adr.sh --body-sha256 <copy>`
#     update available  copy adr-check-version lower than the skill file's
#     tm behind         copy adr-check-version higher than the skill file's
#     up to date        same version, pristine body
#   The skill files are the marker-carrying scripts/*.sh and references/*.yml
#   next to this script; the comparison is header-only. No diff flag: run
#   `diff <copy> <base>/scripts/check_adr.sh` when the diff is wanted.
#
# Usage:
#   adr_status.sh              # from anywhere inside the project checkout
#   adr_status.sh --self-test  # fixtures in mktemp -d, one line per case
#
# Exit: 0 when every copy is `up to date` or `source` (also when no copy
#   exists); 1 any other outcome; 2 usage, bash < 4, no git checkout, or
#   check_adr.sh missing beside this script; 3 no sha256 tool.
#
# Test: `adr_status.sh --self-test` (the five D8 outcomes, the A8 source case,
#   the workflow copy, an untracked copy, a subdirectory run, no copies).

set -euo pipefail
if ((BASH_VERSINFO[0] < 4)); then
  printf 'adr_status.sh: needs bash 4+ (macOS: brew install bash)\n' >&2
  exit 2
fi

this_script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adr_status.sh"
check_adr="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check_adr.sh"
skill_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
marker='# trusty-adr-check'
version_prefix='# adr-check-version: '
hash_prefix='# body-sha256: '

usage() {
  printf 'usage: %s [--self-test]\n' "${0##*/}"
}

# header_field <prefix> <file>: the rest of the first line starting with <prefix>, CR stripped.
header_field() {
  awk -v p="$1" 'index($0, p) == 1 { sub(/\r$/, ""); print substr($0, length(p) + 1); exit }' "$2"
}

has_marker() {
  awk -v m="$marker" '{ sub(/\r$/, "") } $0 == m { found = 1; exit } END { exit !found }' "$1"
}

# version_cmp <a> <b>: prints -1, 0 or 1 as dotted-numeric <a> is below, equal to or above <b>.
version_cmp() {
  local -a a b
  local i x y
  IFS=. read -r -a a <<<"$1"
  IFS=. read -r -a b <<<"$2"
  for ((i = 0; i < ${#a[@]} || i < ${#b[@]}; i++)); do
    x=$((10#${a[i]:-0}))
    y=$((10#${b[i]:-0}))
    if ((x < y)); then printf -- '-1\n'; return; fi
    if ((x > y)); then printf '1\n'; return; fi
  done
  printf '0\n'
}

is_version() {
  [[ "$1" =~ ^[0-9]+([.][0-9]+)*$ ]]
}

# ---- status -----------------------------------------------------------------

declare -A skill_files=()
attention=0
copies=0

report() { # <path> <outcome> <detail>
  printf 'adr-status: %s: %s (%s)\n' "$1" "$2" "$3"
  copies=$((copies + 1))
  case "$2" in "up to date"|source) ;; *) attention=$((attention + 1)) ;; esac
}

# load_skill_files: every marker-carrying scripts/*.sh and references/*.yml beside this script.
load_skill_files() {
  local file
  if [[ ! -f "$check_adr" ]]; then
    printf 'adr-status: %s is missing; the deployed skill is incomplete\n' "$check_adr" >&2
    return 2
  fi
  shopt -s nullglob
  for file in "$skill_dir"/scripts/*.sh "$skill_dir"/references/*.yml; do
    has_marker "$file" || continue
    skill_files["${file##*/}"]="$file"
  done
  shopt -u nullglob
}

# classify <root-relative path>: print the copy's one line.
classify() {
  local path="$1" base skill copy_version copy_hash body_hash skill_version
  base="${path##*/}"
  if [[ "$path" == content/skills/tm-adr/* ]]; then
    report "$path" "source" "the skill's own file; never offered an update PR"
    return
  fi
  if ! has_marker "$path"; then
    report "$path" "unmanaged" "no $marker header; the project maintains the file"
    return
  fi
  copy_version="$(header_field "$version_prefix" "$path")"
  copy_hash="$(header_field "$hash_prefix" "$path")"
  if [[ -z "$copy_version" || -z "$copy_hash" ]]; then
    report "$path" "unmanaged" "incomplete header: adr-check-version and body-sha256 are both required"
    return
  fi
  if ! is_version "$copy_version"; then
    report "$path" "unmanaged" "adr-check-version is not dotted-numeric: $copy_version"
    return
  fi
  skill="${skill_files[$base]:-}"
  if [[ -z "$skill" ]]; then
    report "$path" "unmanaged" "the skill ships no $base to compare against"
    return
  fi
  body_hash="$("$BASH" "$check_adr" --body-sha256 "$path")" || exit $?
  if [[ "$copy_hash" != "$body_hash" ]]; then
    report "$path" "locally edited" "body differs from its header hash; never overwritten"
    return
  fi
  skill_version="$(header_field "$version_prefix" "$skill")"
  case "$(version_cmp "$copy_version" "$skill_version")" in
    -1) report "$path" "update available" "copy $copy_version, skill $skill_version" ;;
    1) report "$path" "tm behind" "copy $copy_version, skill $skill_version; update tm's content" ;;
    0) report "$path" "up to date" "$copy_version" ;;
  esac
}

run_status() {
  local root path
  local -a found=()
  load_skill_files || return $?
  root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    printf 'adr-status: %s is not inside a git checkout\n' "$PWD" >&2
    return 2
  }
  cd "$root"
  mapfile -t found < <(git grep -l --untracked "^$marker" -- '*.sh' '*.yml' 2>/dev/null || true)
  for path in docs/adr/check_adr.sh .github/workflows/adr-check.yml; do
    [[ -f "$path" ]] || continue
    case " ${found[*]:-} " in *" $path "*) ;; *) found+=("$path") ;; esac
  done
  if ((${#found[@]} == 0)); then
    printf 'adr-status: no managed copies in %s; nothing to compare\n' "$root"
    return 0
  fi
  while IFS= read -r path; do
    classify "$path"
  done < <(printf '%s\n' "${found[@]}" | sort -u)
  if ((attention > 0)); then
    printf 'adr-status: %d copy(ies); %d need(s) attention\n' "$copies" "$attention" >&2
    return 1
  fi
  printf 'adr-status: %d copy(ies); every copy is up to date or source\n' "$copies"
}

# ---- self-test --------------------------------------------------------------

passes=0
failures=0
ok() { printf '  ok   %s\n' "$1"; passes=$((passes + 1)); }
bad() { printf 'FAIL: %s\n%s\n' "$1" "${2:-}" >&2; failures=$((failures + 1)); }

# set_header <file> <prefix> <value>: rewrite one header line in place.
set_header() {
  awk -v p="$2" -v v="$3" '!done && index($0, p) == 1 { print p v; done = 1; next } { print }' "$1" >"$1.tmp" &&
    mv "$1.tmp" "$1"
}

# restamp <file>: write the body hash into the header via check_adr.sh --body-sha256.
restamp() {
  set_header "$1" "$hash_prefix" "$("$BASH" "$check_adr" --body-sha256 "$1")"
}

# write_workflow <file> <version>: a fake adr-check.yml with the three-line header.
write_workflow() {
  {
    printf '%s\n%s%s\n%s%s\n' "$marker" "$version_prefix" "$2" "$hash_prefix" "$(printf '%064d' 0)"
    cat <<'EOF'
name: ADR check
on:
  pull_request:
jobs:
  adr-check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: bash docs/adr/check_adr.sh
EOF
  } >"$1"
  restamp "$1"
}

# make_skill <dir>: a skill base holding this script, check_adr.sh at 2.0.0 and a fake workflow.
make_skill() {
  mkdir -p "$1/scripts" "$1/references"
  cp "$this_script" "$1/scripts/adr_status.sh"
  cp "$check_adr" "$1/scripts/check_adr.sh"
  set_header "$1/scripts/check_adr.sh" "$version_prefix" 2.0.0
  write_workflow "$1/references/adr-check.yml" 2.0.0
}

# make_repo <dir>: an empty git repository with docs/adr/ and .github/workflows/.
make_repo() {
  mkdir -p "$1/docs/adr" "$1/.github/workflows"
  (cd "$1" && git -c init.defaultBranch=main init -q)
}

# add_copy <repo> <relpath> <source>: copy a skill file into the repo and stage it.
add_copy() {
  mkdir -p "$1/$(dirname "$2")"
  cp "$3" "$1/$2"
  (cd "$1" && git add "$2")
}

# in_repo <repo> <args...>: run the fake-base adr_status.sh from inside the repo.
in_repo() {
  local dir="$1"
  shift
  output="$(cd "$dir" && "$BASH" "$status_script" "$@" 2>&1)" && status=0 || status=$?
}

# expect <label> <exit> <needle> <repo> <args...>: exit code and one output line.
expect() {
  local label="$1" code="$2" needle="$3"
  shift 3
  in_repo "$@"
  if ((status != code)); then bad "$label: exit $status, expected $code" "$output"
  elif grep -qF -- "$needle" <<<"$output"; then ok "$label"
  else bad "$label: expected '$needle' in:" "$output"; fi
}

# expect_unchanged <label> <repo> <snapshot>: the run wrote nothing into the repo.
expect_unchanged() {
  local diff_out
  diff_out="$(diff -rq -x .git "$2" "$3" 2>&1)" && ok "$1" || bad "$1" "$diff_out"
}

run_selftest() {
  local f output status skill copy wf
  tmp_root="$(mktemp -d)"
  trap 'rm -rf "$tmp_root"' RETURN
  skill="$tmp_root/skill"
  make_skill "$skill"
  status_script="$skill/scripts/adr_status.sh"
  copy=docs/adr/check_adr.sh
  wf=.github/workflows/adr-check.yml

  f="$tmp_root/pristine"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  expect "pristine copy is up to date" 0 "adr-status: $copy: up to date (2.0.0)" "$f"

  f="$tmp_root/lowered"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  set_header "$f/$copy" "$version_prefix" 1.9.9
  expect "lowered adr-check-version is update available" 1 \
    "adr-status: $copy: update available (copy 1.9.9, skill 2.0.0)" "$f"

  f="$tmp_root/edited"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  awk '!done && sub(/^errors=0$/, "errors=1") { done = 1 } { print }' "$skill/scripts/check_adr.sh" >"$f/$copy"
  cp -R "$f" "$f.before"
  expect "one body byte changed is locally edited" 1 "adr-status: $copy: locally edited" "$f"
  expect_unchanged "locally edited copy: nothing written" "$f" "$f.before"

  f="$tmp_root/noheader"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  awk 'NR < 2 || NR > 4 { print }' "$skill/scripts/check_adr.sh" >"$f/$copy"
  expect "header removed at the fixed path is unmanaged" 1 "adr-status: $copy: unmanaged" "$f"

  f="$tmp_root/raised"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  set_header "$f/$copy" "$version_prefix" 2.0.1
  cp -R "$f" "$f.before"
  expect "raised adr-check-version is tm behind" 1 "adr-status: $copy: tm behind (copy 2.0.1, skill 2.0.0" "$f"
  expect_unchanged "tm behind copy: nothing written" "$f" "$f.before"

  f="$tmp_root/numeric"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  set_header "$f/$copy" "$version_prefix" 10.0.0
  expect "version compare is numeric: 10.0.0 is above 2.0.0" 1 "adr-status: $copy: tm behind" "$f"

  f="$tmp_root/source"; make_repo "$f"
  add_copy "$f" content/skills/tm-adr/scripts/check_adr.sh "$skill/scripts/check_adr.sh"
  set_header "$f/content/skills/tm-adr/scripts/check_adr.sh" "$version_prefix" 1.0.0
  add_copy "$f" content/skills/tm-adr/references/adr-check.yml "$skill/references/adr-check.yml"
  expect "A8: a copy under content/skills/tm-adr/ is source" 0 \
    "adr-status: content/skills/tm-adr/scripts/check_adr.sh: source" "$f"
  if grep -qF "update available" <<<"$output"; then bad "A8: source copy was offered an update" "$output"
  else ok "A8: a source copy is never offered an update PR"; fi

  f="$tmp_root/workflow"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  add_copy "$f" "$wf" "$skill/references/adr-check.yml"
  expect "pristine workflow copy is up to date" 0 "adr-status: $wf: up to date (2.0.0)" "$f"
  if [[ "$(grep -c '^adr-status: .*: ' <<<"$output")" -eq 2 ]]; then ok "one line per copy"
  else bad "one line per copy" "$output"; fi
  set_header "$f/$wf" "$version_prefix" 1.0.0
  expect "lowered workflow copy is update available" 1 "adr-status: $wf: update available (copy 1.0.0, skill 2.0.0)" "$f"
  expect "a mixed run still reports the up-to-date copy" 1 "adr-status: $copy: up to date (2.0.0)" "$f"
  awk 'NR > 3 { print }' "$skill/references/adr-check.yml" >"$f/$wf"
  expect "workflow without the marker at the fixed path is unmanaged" 1 "adr-status: $wf: unmanaged" "$f"

  f="$tmp_root/untracked"; make_repo "$f"; mkdir -p "$f/tools"; cp "$skill/scripts/check_adr.sh" "$f/tools/check_adr.sh"
  expect "an untracked copy is found" 0 "adr-status: tools/check_adr.sh: up to date (2.0.0)" "$f"

  f="$tmp_root/unknown"; make_repo "$f"; add_copy "$f" tools/other.sh "$skill/scripts/check_adr.sh"
  expect "a marked copy with no skill file of that basename is unmanaged" 1 \
    "adr-status: tools/other.sh: unmanaged (the skill ships no other.sh" "$f"

  f="$tmp_root/subdir"; make_repo "$f"; add_copy "$f" "$copy" "$skill/scripts/check_adr.sh"
  expect "a run from a subdirectory reports root-relative paths" 0 "adr-status: $copy: up to date (2.0.0)" "$f/docs"

  f="$tmp_root/none"; make_repo "$f"
  in_repo "$f"
  if ((status == 0)) && [[ "$(wc -l <<<"$output")" -eq 1 ]] && grep -qF 'no managed copies' <<<"$output"; then
    ok "no copies: exit 0 with one notice line"
  else bad "no copies (exit $status)" "$output"; fi

  f="$tmp_root/nogit"; mkdir -p "$f"
  expect "outside a git checkout exits 2" 2 "is not inside a git checkout" "$f"
  expect "an unknown flag exits 2" 2 "usage:" "$tmp_root/pristine" --diff

  if ((failures > 0)); then
    printf 'adr_status self-test: %d/%d case(s) failed\n' "$failures" "$((passes + failures))" >&2
    return 1
  fi
  printf 'adr_status self-test: all %d cases passed\n' "$passes"
}

# ---- entry ------------------------------------------------------------------

case "${1:-}" in
  "") run_status ;;
  --self-test) run_selftest ;;
  -h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
