#!/usr/bin/env bash
#
# adr_init.sh — bootstrap docs/adr/ in a project with no ADR setup, as the
# staged contents of one pull request.
#
# Why: a project adopts ADRs once. Done by hand, the seed files drift from
#   the standard and the check script gets re-typed, so it is no longer the
#   byte-identical managed copy that `/tm-adr status` can update later.
#
# What: from inside the target project's git worktree, stops when the project
#   already has an ADR setup (an ADR directory other than docs/adr/, an
#   NNNN-*.md containing `## Decision`, an NNNN-*.md under docs/adr/, another
#   ADR tool's marker file, an ADR mention in CLAUDE.md), naming each marker. Otherwise writes docs/adr/{README.md, template.md, INDEX.md,
#   0001-record-architecture-decisions.md} from the skill's references/ with
#   {{DATE}} rendered, copies check_adr.sh with cp and proves it with cmp
#   (mode 100755), adds a .gitattributes eol=lf line for it, and, when
#   .github/workflows/ exists, renders references/adr-check.yml to the default
#   branch and restamps its body hash; otherwise appends a run-it note to the
#   README it wrote. A file already present is kept byte-identical and named.
#   Stages what it writes; never commits. Finds the source files relative to
#   its own location only.
#
# Usage:
#   adr_init.sh              # from inside the target project's git worktree
#   adr_init.sh --self-test  # heredoc fixtures in mktemp -d
#
# Exit: 0 installed and staged; 1 an existing ADR setup was found (nothing
#   written) or the installed check fails; 2 usage, bash < 4, not inside a git
#   worktree, the file listing failed, or the deployed skill is incomplete.
#   An NNNN-*.md or a CLAUDE.md the scan cannot read counts as a marker (exit 1).

set -euo pipefail
if ((BASH_VERSINFO[0] < 4)); then
  printf 'adr_init.sh: needs bash 4+ (macOS: brew install bash)\n' >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check_src="$script_dir/check_adr.sh"
refs_dir="$(dirname "$script_dir")/references"
this_init="$script_dir/adr_init.sh"
seed_files=(README.md template.md INDEX.md 0001-record-architecture-decisions.md)
workflow_rel=.github/workflows/adr-check.yml
check_rel=docs/adr/check_adr.sh
# Marker files other ADR tools write; detection only, never written here.
tool_marker_files=(.adr-dir .log4brains.yml)

usage() {
  printf 'usage: %s            # from inside the target project git worktree\n' "${0##*/}"
  printf '       %s --self-test\n' "${0##*/}"
}

say() { printf 'adr-init: %s\n' "$*"; }
complain() { printf 'adr-init: %s\n' "$*" >&2; }

# ---- preconditions ----------------------------------------------------------

require_source() {
  local f
  if [[ ! -f "$check_src" ]]; then
    complain "$check_src is absent: the deployed skill is incomplete, and a re-typed script would not be byte-identical; nothing written"
    return 2
  fi
  for f in "${seed_files[@]}" adr-check.yml; do
    if [[ ! -f "$refs_dir/$f" ]]; then
      complain "$refs_dir/$f is absent: the deployed skill is incomplete; nothing written"
      return 2
    fi
  done
}

# Prints the project root; fails outside a git worktree.
project_root() {
  local root
  if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    complain "$PWD is not inside a git worktree; init must run in a worktree (ADR-0044); nothing written"
    return 2
  fi
  printf '%s\n' "$root"
}

# ---- detection --------------------------------------------------------------

# Lists every existing-setup marker, one per line. Runs from the project root
# over the tracked and the untracked-not-ignored files. Fails closed (#10080):
# a listing that cannot be produced is exit 2, never "no markers", and an
# NNNN-*.md or CLAUDE.md that cannot be read is a marker, never an absence.
list_markers() {
  local path dir base part prefix claude line marker listing ls_status grep_status
  local -A seen_dir=()
  listing="$(git ls-files --cached --others --exclude-standard)" && ls_status=0 || ls_status=$?
  if ((ls_status != 0)); then
    complain "git ls-files exited $ls_status; cannot tell whether an ADR setup exists; nothing written"
    return 2
  fi
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    base="${path##*/}"
    dir="${path%/*}"
    [[ "$dir" != "$path" ]] || dir=""
    prefix=""
    if [[ -n "$dir" ]]; then
      while IFS= read -r -d / part; do
        prefix="${prefix:+$prefix/}$part"
        case "$part" in
          adr|decisions|architecture-decisions)
            if [[ "$prefix" != docs/adr && -z "${seen_dir[$prefix]:-}" ]]; then
              seen_dir[$prefix]=1
              printf 'directory %s/ (an ADR directory other than docs/adr/)\n' "$prefix"
            fi ;;
        esac
      done <<<"$dir/"
    fi
    for marker in "${tool_marker_files[@]}"; do
      if [[ "$base" == "$marker" ]]; then
        printf '%s (a %s file)\n' "$path" "$marker"
      fi
    done
    if [[ "$base" =~ ^[0-9]{4}-.*[.]md$ ]]; then
      if [[ "$dir" == docs/adr ]]; then
        printf '%s (an ADR already under docs/adr/)\n' "$path"
      else
        grep -qF '## Decision' -- "$path" 2>/dev/null && grep_status=0 || grep_status=$?
        case "$grep_status" in
          0) printf '%s (an NNNN-*.md containing "## Decision")\n' "$path" ;;
          1) ;;
          *) printf '%s (an NNNN-*.md that could not be read, grep exit %s; treated as a marker)\n' "$path" "$grep_status" ;;
        esac
      fi
    fi
  done <<<"$listing"
  # #10080: -L keeps a dangling symlink in the scan; -m 1 replaces `| head`,
  # whose SIGPIPE would reach the status under pipefail.
  for claude in CLAUDE.md .claude/CLAUDE.md; do
    [[ -f "$claude" || -L "$claude" ]] || continue
    line="$(grep -m 1 -niwE 'adrs?|architecture[- ]decisions?' -- "$claude" 2>/dev/null)" && grep_status=0 || grep_status=$?
    case "$grep_status" in
      0) printf '%s line %s (an ADR mention in CLAUDE.md)\n' "$claude" "${line%%:*}" ;;
      1) ;;
      *) printf '%s (a CLAUDE.md that could not be read, grep exit %s; treated as a marker)\n' "$claude" "$grep_status" ;;
    esac
  done
}

# ---- rendering and writing --------------------------------------------------

# Prints the project's default branch: origin/HEAD, else a local main or
# master, else the current branch, else main.
detect_default_branch() {
  local ref
  if ref="$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)"; then
    printf '%s\n' "${ref#origin/}"
    return
  fi
  for ref in main master; do
    if git show-ref --verify --quiet "refs/heads/$ref"; then
      printf '%s\n' "$ref"
      return
    fi
  done
  if ref="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)" && [[ -n "$ref" ]]; then
    printf '%s\n' "$ref"
    return
  fi
  printf 'main\n'
}

# render <src> <dst>: copy with {{DATE}} and {{DEFAULT_BRANCH}} rendered.
render() {
  local content
  content="$(<"$1")"
  content="${content//\{\{DATE\}\}/$today}"
  content="${content//\{\{DEFAULT_BRANCH\}\}/$default_branch}"
  printf '%s\n' "$content" >"$2"
}

# restamp <file>: rewrite the body-sha256 header line from the file's body.
restamp() {
  local file="$1" hash
  hash="$("$BASH" "$check_src" --body-sha256 "$file")"
  awk -v h="$hash" '!done && sub(/^# body-sha256: .*$/, "# body-sha256: " h) { done = 1 } { print }' \
    "$file" >"$file.tmp" && mv "$file.tmp" "$file"
}

append_runit_note() {
  cat >>"$1" <<'EOF'

## Running the check in CI

The project had no `.github/workflows/` directory when this setup was installed, so no
workflow was added. Run `bash docs/adr/check_adr.sh` in CI on every pull request and on
every push to the default branch; the `tm-adr` skill's `references/adr-check.yml` is a
GitHub Actions example of that job.
EOF
}

# Writes the seed, from the project root. Prints one line per file.
install_seed() {
  local f dst readme_written=0 check_written=0 note="" check_out
  local -a written=()
  today="$(date +%F)"
  default_branch="$(detect_default_branch)"

  mkdir -p docs/adr
  for f in "${seed_files[@]}"; do
    dst="docs/adr/$f"
    if [[ -e "$dst" ]]; then
      say "kept: $dst (already present; left byte-identical)"
    else
      render "$refs_dir/$f" "$dst"
      written+=("$dst")
      [[ "$f" != README.md ]] || readme_written=1
    fi
  done

  if [[ -e "$check_rel" ]]; then
    say "kept: $check_rel (already present; left byte-identical)"
  else
    cp "$check_src" "$check_rel"
    chmod 755 "$check_rel"
    if ! cmp -s "$check_src" "$check_rel"; then
      complain "$check_rel differs from $check_src after cp; aborting before staging"
      return 1
    fi
    written+=("$check_rel")
    check_written=1
  fi

  if [[ -f .gitattributes ]] && grep -qE '^docs/adr/check_adr[.]sh[[:space:]]' .gitattributes; then
    say "kept: .gitattributes (already has a docs/adr/check_adr.sh line)"
  else
    if [[ -s .gitattributes && -n "$(tail -c 1 .gitattributes)" ]]; then
      printf '\n' >>.gitattributes
    fi
    printf 'docs/adr/check_adr.sh text eol=lf\n' >>.gitattributes
    written+=(.gitattributes)
  fi

  if [[ -d .github/workflows ]]; then
    if [[ -e "$workflow_rel" ]]; then
      say "kept: $workflow_rel (already present; left byte-identical)"
    else
      render "$refs_dir/adr-check.yml" "$workflow_rel"
      restamp "$workflow_rel"
      written+=("$workflow_rel")
    fi
  else
    note="no .github/workflows/ directory, so no workflow was installed; run 'bash docs/adr/check_adr.sh' in CI on every pull request and every push to $default_branch"
    if ((readme_written)); then
      append_runit_note docs/adr/README.md
      note="$note (note appended to docs/adr/README.md)"
    fi
  fi

  for f in "${written[@]}"; do
    say "written: $f"
  done
  if ((${#written[@]})); then
    git add -- "${written[@]}"
    if ((check_written)); then
      git update-index --chmod=+x -- "$check_rel"
    fi
  fi
  say "staged: ${#written[@]} file(s); nothing committed"
  [[ -z "$note" ]] || say "note: $note"

  if ((check_written)); then
    "$BASH" "$check_src" --verify-self "$check_rel" >/dev/null
  fi
  if [[ -f "$workflow_rel" ]] && grep -qE '^# trusty-adr-check' "$workflow_rel"; then
    "$BASH" "$check_src" --verify-self "$workflow_rel" >/dev/null
  fi
  if ! check_out="$("$BASH" "$check_src")"; then
    complain "the installed check fails on the staged tree; fix it before committing"
    return 1
  fi
  say "check: $check_out"
  say "next: commit the staged files in this worktree; version-control pushes and opens the PR"
}

run_init() {
  local root found
  local -a markers
  require_source || return $?
  root="$(project_root)" || return $?
  cd "$root"
  # #10080: capture the status too; `mapfile < <(...)` discards it.
  found="$(list_markers)" || return $?
  markers=()
  [[ -z "$found" ]] || mapfile -t markers <<<"$found"
  if ((${#markers[@]})); then
    complain "existing ADR setup found; nothing written:"
    printf '  marker: %s\n' "${markers[@]}" >&2
    return 1
  fi
  install_seed
}

# ---- self-test --------------------------------------------------------------

passes=0
failures=0
skips=0
ok() { printf '  ok   %s\n' "$1"; passes=$((passes + 1)); }
bad() { printf 'FAIL: %s\n%s\n' "$1" "${2:-}" >&2; failures=$((failures + 1)); }
# A case the environment cannot exercise; counted apart from the passes.
skip() { printf '  skip %s\n' "$1"; skips=$((skips + 1)); }

# new_repo <dir> [default-branch]: an empty git repository.
new_repo() {
  mkdir -p "$1"
  (cd "$1" && git -c init.defaultBranch="${2:-trunk}" init -q)
}

# commit_all <dir>: commit everything so `git status --porcelain` is empty.
commit_all() {
  (cd "$1" && git add . && git -c user.name=t -c user.email=t@example.invalid commit -qm seed)
}

# in_repo <dir> <command...>: run a command inside a fixture; capture output.
in_repo() {
  local dir="$1"
  shift
  output="$(cd "$dir" && "$@" 2>&1)" && status=0 || status=$?
}

# expect_marker <label> <marker-text> <dir>: init stops, names the marker,
# writes nothing.
expect_marker() {
  local label="$1" marker="$2" dir="$3" before after
  before="$(cd "$dir" && git status --porcelain)"
  in_repo "$dir" "$BASH" "$this_init"
  after="$(cd "$dir" && git status --porcelain)"
  if ((status != 1)); then bad "$label: exit $status, expected 1" "$output"
  elif ! grep -qF -- "$marker" <<<"$output"; then bad "$label: output does not name '$marker'" "$output"
  elif [[ "$before" != "$after" ]]; then bad "$label: the tree changed" "$after"
  else ok "$label"; fi
}

check_fresh_with_workflows() {
  local f="$1" placeholder
  new_repo "$f" trunk
  mkdir -p "$f/.github/workflows"
  printf '# Project\n\nUse the address book ladder.\n' >"$f/CLAUDE.md"
  commit_all "$f"
  in_repo "$f" "$BASH" "$this_init"
  if ((status == 0)); then ok "fresh repo with .github/workflows/: init exits 0"
  else bad "fresh repo with .github/workflows/ (exit $status)" "$output"; return; fi
  for placeholder in docs/adr/README.md docs/adr/template.md docs/adr/INDEX.md \
      docs/adr/0001-record-architecture-decisions.md docs/adr/check_adr.sh .gitattributes "$workflow_rel"; do
    [[ -f "$f/$placeholder" ]] || { bad "fresh repo: $placeholder is missing" "$output"; return; }
  done
  ok "fresh repo: the full file set is present"
  if cmp -s "$f/$check_rel" "$check_src"; then ok "fresh repo: cmp docs/adr/check_adr.sh against the skill copy exits 0"
  else bad "fresh repo: check_adr.sh is not byte-identical"; fi
  in_repo "$f" git ls-files -s -- "$check_rel"
  if [[ "$output" == 100755* ]]; then ok "fresh repo: git ls-files -s shows 100755 for check_adr.sh"
  else bad "fresh repo: ls-files mode" "$output"; fi
  in_repo "$f" git status --porcelain
  if ! grep -q '^??' <<<"$output" && grep -q '^A ' <<<"$output"; then ok "fresh repo: every written file is staged, none untracked"
  else bad "fresh repo: staging" "$output"; fi
  in_repo "$f" git rev-list --count HEAD
  if [[ "$output" == 1 ]]; then ok "fresh repo: nothing committed beyond the fixture's seed commit"
  else bad "fresh repo: commit count" "$output"; fi
  if grep -rqE '\{\{(DATE|DEFAULT_BRANCH)\}\}' "$f/docs/adr" "$f/$workflow_rel"; then
    bad "fresh repo: a placeholder survived rendering" "$(grep -rnE '\{\{(DATE|DEFAULT_BRANCH)\}\}' "$f/docs/adr" "$f/$workflow_rel")"
  else ok "fresh repo: {{DATE}} and {{DEFAULT_BRANCH}} are rendered"; fi
  if grep -qF -- "- **Date:** $(date +%F)" "$f/docs/adr/0001-record-architecture-decisions.md"; then ok "fresh repo: 0001 is dated today"
  else bad "fresh repo: 0001 date" "$(grep -F 'Date:' "$f/docs/adr/0001-record-architecture-decisions.md")"; fi
  if grep -qF 'branches: ["trunk"]' "$f/$workflow_rel"; then ok "fresh repo: adr-check.yml is rendered to the default branch"
  else bad "fresh repo: workflow branch" "$(grep -F branches "$f/$workflow_rel")"; fi
  if [[ ! -e "$f/docs/adr/adr-standard.md" ]]; then ok "fresh repo: adr-standard.md is not copied into the project"
  else bad "fresh repo: adr-standard.md was written"; fi
  in_repo "$f" "$BASH" "$check_rel"
  if ((status == 0)); then ok "fresh repo: bash docs/adr/check_adr.sh exits 0 on the bootstrapped tree"
  else bad "fresh repo: check (exit $status)" "$output"; fi
  in_repo "$f" "$BASH" "$check_rel" --verify-self "$check_rel"
  if ((status == 0)); then ok "fresh repo: --verify-self passes on the installed check_adr.sh"
  else bad "fresh repo: verify check (exit $status)" "$output"; fi
  in_repo "$f" "$BASH" "$check_rel" --verify-self "$workflow_rel"
  if ((status == 0)); then ok "fresh repo: --verify-self passes on the rendered adr-check.yml"
  else bad "fresh repo: verify workflow (exit $status)" "$output"; fi
  expect_marker "a second run stops on the ADR already under docs/adr/" \
    "docs/adr/0001-record-architecture-decisions.md (an ADR already under docs/adr/)" "$f"
}

check_fresh_without_github() {
  local f="$1"
  new_repo "$f"
  in_repo "$f" "$BASH" "$this_init"
  if ((status != 0)); then bad "fresh repo without .github/ (exit $status)" "$output"; return; fi
  if [[ ! -e "$f/.github" ]] && grep -qF 'note: no .github/workflows/ directory' <<<"$output"; then
    ok "fresh repo without .github/: no workflow, run-it note in the output"
  else bad "fresh repo without .github/: workflow or note" "$output"; fi
  if grep -qF '## Running the check in CI' "$f/docs/adr/README.md"; then ok "fresh repo without .github/: run-it note appended to README.md"
  else bad "fresh repo without .github/: README note"; fi
  in_repo "$f" "$BASH" "$check_rel"
  if ((status == 0)); then ok "fresh repo without .github/: bash docs/adr/check_adr.sh exits 0"
  else bad "fresh repo without .github/: check (exit $status)" "$output"; fi
}

check_existing_docs_adr() {
  local f="$1" readme='# Our decisions

House rules live here.
'
  new_repo "$f"
  mkdir -p "$f/docs/adr"
  printf '%s' "$readme" >"$f/docs/adr/README.md"
  commit_all "$f"
  in_repo "$f" "$BASH" "$this_init"
  if ((status != 0)); then bad "existing docs/adr/ with a differing README (exit $status)" "$output"; return; fi
  if [[ "$(<"$f/docs/adr/README.md")" == "${readme%$'\n'}" ]] && grep -qF 'kept: docs/adr/README.md' <<<"$output"; then
    ok "existing docs/adr/: the differing README.md is kept byte-identical and named"
  else bad "existing docs/adr/: README" "$output"; fi
  if [[ -f "$f/docs/adr/INDEX.md" && -f "$f/$check_rel" && -f "$f/docs/adr/0001-record-architecture-decisions.md" ]]; then
    ok "existing docs/adr/: the missing files are written"
  else bad "existing docs/adr/: missing files" "$output"; fi
  in_repo "$f" "$BASH" "$check_rel"
  if ((status == 0)); then ok "existing docs/adr/: bash docs/adr/check_adr.sh exits 0"
  else bad "existing docs/adr/: check (exit $status)" "$output"; fi
}

check_markers() {
  local f
  local tool_file="${tool_marker_files[1]}" dir_file="${tool_marker_files[0]}"
  f="$1/log4brains"; new_repo "$f"; printf 'project:\n  name: x\n' >"$f/$tool_file"; commit_all "$f"
  expect_marker "$tool_file only: nothing written, marker named" "$tool_file (a $tool_file file)" "$f"
  in_repo "$f" git status --porcelain
  if [[ -z "$output" ]]; then ok "$tool_file only: git status --porcelain is empty"
  else bad "$tool_file only: porcelain" "$output"; fi
  f="$1/adrdir"; new_repo "$f"; mkdir -p "$f/adr"; printf 'x\n' >"$f/adr/notes.md"
  expect_marker "an adr/ directory outside docs/adr/" "directory adr/ (an ADR directory other than docs/adr/)" "$f"
  f="$1/archdir"; new_repo "$f"; mkdir -p "$f/docs/architecture-decisions"; printf 'x\n' >"$f/docs/architecture-decisions/README.md"
  expect_marker "a nested architecture-decisions/ directory" "directory docs/architecture-decisions/" "$f"
  f="$1/decision"; new_repo "$f"; mkdir -p "$f/notes"; printf '# 0007. Pick a db\n\n## Decision\n\nPostgres.\n' >"$f/notes/0007-pick-a-db.md"
  expect_marker "an NNNN-*.md containing ## Decision" 'notes/0007-pick-a-db.md (an NNNN-*.md containing "## Decision")' "$f"
  f="$1/adrdirfile"; new_repo "$f"; printf 'doc/architecture\n' >"$f/$dir_file"
  expect_marker "a $dir_file file" "$dir_file (a $dir_file file)" "$f"
  f="$1/claude"; new_repo "$f"; printf '# Project\n\nDecisions are ADRs under doc/decisions.\n' >"$f/CLAUDE.md"
  expect_marker "an ADR mention in CLAUDE.md" "CLAUDE.md line 3 (an ADR mention in CLAUDE.md)" "$f"
}

check_refusals() {
  local f skill
  f="$1/nogit"; mkdir -p "$f"
  in_repo "$f" "$BASH" "$this_init"
  if ((status == 2)) && grep -qF 'not inside a git worktree' <<<"$output"; then ok "outside a git worktree: exit 2 names the reason"
  else bad "outside a git worktree (exit $status)" "$output"; fi
  skill="$1/skill"; mkdir -p "$skill/scripts" "$skill/references"
  cp "$this_init" "$skill/scripts/adr_init.sh"; cp "$refs_dir"/* "$skill/references/"
  f="$1/nosource"; new_repo "$f"
  in_repo "$f" "$BASH" "$skill/scripts/adr_init.sh"
  if ((status == 2)) && grep -qF 'check_adr.sh is absent' <<<"$output" && [[ -z "$(cd "$f" && git status --porcelain)" ]]; then
    ok "skill without scripts/check_adr.sh: exit 2 names the missing file, nothing written"
  else bad "skill without check_adr.sh (exit $status)" "$output"; fi
  in_repo "$1" "$BASH" "$this_init" --bogus
  if ((status == 2)); then ok "an unknown flag exits 2"; else bad "unknown flag: exit $status" "$output"; fi
}

# #10080: the marker scan fails closed — a listing that fails is exit 2 with
# nothing written; an NNNN-*.md that cannot be read is a marker.
check_fail_closed() {
  local f adr
  f="$1/corrupt"; new_repo "$f"; printf 'x\n' >"$f/notes.md"
  printf 'garbage\n' >"$f/.git/index"
  in_repo "$f" "$BASH" "$this_init"
  if ((status == 2)) && grep -qF 'git ls-files exited' <<<"$output" && [[ ! -e "$f/docs" && ! -e "$f/.gitattributes" ]]; then
    ok "a corrupt git index: exit 2 names git ls-files, nothing written"
  else bad "a corrupt git index (exit $status)" "$output"; fi
  f="$1/unreadable"; new_repo "$f"; mkdir -p "$f/notes"
  adr="$f/notes/0007-pick-a-db.md"
  printf '# 0007. Pick a db\n\nNothing decided yet.\n' >"$adr"
  chmod 000 "$adr"
  if [[ -r "$adr" ]]; then
    skip "an unreadable NNNN-*.md: a mode-000 file stays readable to this user"
  else
    expect_marker "an unreadable NNNN-*.md is a marker, not an absence" \
      'notes/0007-pick-a-db.md (an NNNN-*.md that could not be read, grep exit 2; treated as a marker)' "$f"
  fi
  chmod 644 "$adr"
  # The CLAUDE.md has no ADR mention: unreadable alone must stop init.
  f="$1/claude000"; new_repo "$f"; printf '# Project\n\nUse the address book ladder.\n' >"$f/CLAUDE.md"
  chmod 000 "$f/CLAUDE.md"
  if [[ -r "$f/CLAUDE.md" ]]; then
    skip "an unreadable CLAUDE.md: a mode-000 file stays readable to this user"
  else
    expect_marker "an unreadable CLAUDE.md is a marker, not an absence" \
      'CLAUDE.md (a CLAUDE.md that could not be read, grep exit 2; treated as a marker)' "$f"
  fi
  chmod 644 "$f/CLAUDE.md"
}

run_selftest() {
  local tmp_root output status
  tmp_root="$(mktemp -d)"
  trap 'rm -rf "$tmp_root"' RETURN
  check_fresh_with_workflows "$tmp_root/fresh"
  check_fresh_without_github "$tmp_root/nogithub"
  check_existing_docs_adr "$tmp_root/existing"
  check_markers "$tmp_root"
  check_refusals "$tmp_root"
  check_fail_closed "$tmp_root"
  if ((failures > 0)); then
    printf 'adr_init self-test: %d/%d case(s) failed, %d skipped\n' "$failures" "$((passes + failures))" "$skips" >&2
    return 1
  fi
  printf 'adr_init self-test: all %d cases passed, %d skipped\n' "$passes" "$skips"
}

# ---- entry ------------------------------------------------------------------

today=""
default_branch=""
case "${1:-}" in
  "") run_init ;;
  --self-test) [[ $# -eq 1 ]] || { usage >&2; exit 2; }; run_selftest ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac
