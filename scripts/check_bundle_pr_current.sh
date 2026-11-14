#!/usr/bin/env bash
#
# check_bundle_pr_current.sh — an Architect-bundle PR must contain main's tip
# (issue #11120; owner ruling 2026-11-14, "bundle PRs must be current with main").
#
# Why: a bundle PR regenerates python/trusty-architect/provenance.json, the
#   asset copy of it and seed_history.tsv. Two bundle PRs cut from one base are
#   each green alone; merged one after the other, git combines their generated
#   files into bytes neither generated, and main goes red (#11236 + #11191,
#   repaired by #11246). Only a branch that already contains main's tip
#   regenerated those files against everything main holds.
# What: lists the files the PR changes, then
#   - PASS when none is an Architect bundle path (BUNDLE_PATHS below);
#   - else PASS when <base> is an ancestor of <head>, and FAIL BEHIND, with the
#     regeneration commands, when it is not.
#   The changed files come from --files (one path per line, `-` for stdin),
#   from --range (`git diff --no-renames`), or by default from
#   `git diff --no-renames <base>...<head>`, the PR's own changes. --no-renames
#   lists both sides of a rename, so a rename into or out of the bundle counts.
#   The diff is read with -z, so a path git would C-quote (non-ASCII, a
#   newline) is matched as its real bytes.
#   --fetch <remote> is the CI mode: --base is then a branch name. The script
#   fetches that branch into refs/remotes/<remote>/<base>, fetches the head
#   SHA when it is absent, and verifies each revision on its own.
#   Fails closed: a failed fetch, an unresolvable head or base, or a failed
#   diff exits 2.
# Test: scripts/check_bundle_pr_current_selftest.sh
#
# Usage: check_bundle_pr_current.sh [--repo DIR] [--fetch <remote>]
#          --head <sha> --base <ref> [--files <file|-> | --range <a..b>]
# Exit: 0 pass, 1 behind main, 2 usage or git error.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git only.

set -euo pipefail

# The Architect bundle: every file scripts/architect_bundle_meta.py hashes,
# reads or writes (its ROOT..FIXTURES constants, lines 32-41), the content
# copies the pair list in scripts/check_architect_subproject.sh ships, that
# script itself (its pair table drives the provenance), and the pinned skill
# hashes. A trailing `/` is a directory prefix; anything else
# is one exact file.
BUNDLE_PATHS=(
  "python/trusty-architect/"
  "crates/trusty-mpm/src/assets/architect/"
  "content/instructions/architect/"
  "content/skills/tm-architect-setup.md"
  "scripts/architect_bundle_meta.py"
  "scripts/architect_export_manifest.txt"
  "scripts/check_architect_subproject.sh"
  "crates/trusty-mpm/src/bin/tm/commands/fleet/seed_table.rs"
  "crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv"
  "crates/trusty-mpm/src/bin/tm/commands/fleet/il_init_sample_state.md"
  "crates/trusty-mpm/src/core/project_skill_tier/architect_skills.rs"
)

usage() {
  echo "usage: $0 [--repo DIR] [--fetch <remote>] --head <sha> --base <ref> [--files <file|-> | --range <a..b>]" >&2
  exit 2
}

REPO="."
HEAD_REV=""
BASE_REV=""
FILES=""
RANGE=""
REMOTE=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)  [ -n "${2:-}" ] || usage; REPO="$2"; shift 2 ;;
    --head)  [ -n "${2:-}" ] || usage; HEAD_REV="$2"; shift 2 ;;
    --base)  [ -n "${2:-}" ] || usage; BASE_REV="$2"; shift 2 ;;
    --files) [ -n "${2:-}" ] || usage; FILES="$2"; shift 2 ;;
    --range) [ -n "${2:-}" ] || usage; RANGE="$2"; shift 2 ;;
    --fetch) [ -n "${2:-}" ] || usage; REMOTE="$2"; shift 2 ;;
    *) usage ;;
  esac
done
[ -n "$HEAD_REV" ] && [ -n "$BASE_REV" ] || usage
[ -z "$FILES" ] || [ -z "$RANGE" ] || usage

g() { git -C "$REPO" "$@"; }

if [ -n "$REMOTE" ]; then
  # CI mode: --head must be a full SHA, so it can never parse as a git option.
  case "$HEAD_REV" in
    *[!0-9a-f]*) echo "ERROR: --fetch needs --head as a hex SHA, got '$HEAD_REV'" >&2; exit 2 ;;
  esac
  [ "${#HEAD_REV}" -ge 40 ] \
    || { echo "ERROR: --fetch needs --head as a full SHA, got '$HEAD_REV'" >&2; exit 2; }
  g fetch --no-tags --quiet "$REMOTE" "+refs/heads/${BASE_REV}:refs/remotes/${REMOTE}/${BASE_REV}" \
    || { echo "ERROR: cannot fetch branch '$BASE_REV' from '$REMOTE'" >&2; exit 2; }
  if ! g cat-file -e "${HEAD_REV}^{commit}" 2>/dev/null; then
    g fetch --no-tags --quiet "$REMOTE" "$HEAD_REV" \
      || { echo "ERROR: cannot fetch head '$HEAD_REV' from '$REMOTE'" >&2; exit 2; }
  fi
  BASE_REV="refs/remotes/${REMOTE}/${BASE_REV}"
fi

head_sha="$(g rev-parse --verify --quiet "${HEAD_REV}^{commit}")" \
  || { echo "ERROR: head '$HEAD_REV' does not resolve to a commit" >&2; exit 2; }
base_sha="$(g rev-parse --verify --quiet "${BASE_REV}^{commit}")" \
  || { echo "ERROR: base '$BASE_REV' does not resolve to a commit" >&2; exit 2; }

# The changed paths, NUL-delimited, in a temp file: command substitution
# drops NUL bytes, and -z is what keeps git from C-quoting a path.
changed="$(mktemp)"
trap 'rm -f "$changed"' EXIT
if [ -n "$FILES" ]; then
  if [ "$FILES" = "-" ]; then
    tr '\n' '\0' > "$changed"
  else
    [ -r "$FILES" ] || { echo "ERROR: cannot read --files '$FILES'" >&2; exit 2; }
    tr '\n' '\0' < "$FILES" > "$changed"
  fi
else
  [ -n "$RANGE" ] || RANGE="${base_sha}...${head_sha}"
  g diff -z --no-renames --name-only "$RANGE" > "$changed" \
    || { echo "ERROR: git diff $RANGE failed" >&2; exit 2; }
fi

# is_bundle <path>: 0 when <path> is an Architect bundle path.
is_bundle() {
  local p
  for p in "${BUNDLE_PATHS[@]}"; do
    case "$p" in
      */) case "$1" in "$p"*) return 0 ;; esac ;;
      *)  [ "$1" = "$p" ] && return 0 ;;
    esac
  done
  return 1
}

hits=()
while IFS= read -r -d '' f || [ -n "$f" ]; do
  [ -n "$f" ] || continue
  if is_bundle "$f"; then hits+=("$f"); fi
done < "$changed"

if [ "${#hits[@]}" -eq 0 ]; then
  echo "PASS: no Architect bundle file changed; this PR need not be current with ${BASE_REV}."
  exit 0
fi

if g merge-base --is-ancestor "$base_sha" "$head_sha"; then
  echo "PASS: ${#hits[@]} Architect bundle file(s) changed and head ${head_sha} contains ${BASE_REV} ${base_sha}."
  exit 0
fi

cat <<EOF
FAIL BEHIND: this PR changes ${#hits[@]} Architect bundle file(s), and its head
${head_sha} does not contain the tip of ${BASE_REV}, ${base_sha}.

Two bundle PRs from one base merge their regenerated provenance.json and
seed_history.tsv into bytes neither generated (#11120). Take main, regenerate,
check, then push:

  git fetch origin
  git merge origin/main
  python3 scripts/architect_bundle_meta.py provenance
  cp python/trusty-architect/provenance.json crates/trusty-mpm/src/assets/architect/provenance.json
  python3 scripts/architect_bundle_meta.py history
  python3 scripts/architect_bundle_meta.py provenance --check
  python3 scripts/architect_bundle_meta.py history --check
  bash scripts/check_architect_subproject.sh
  git add python/trusty-architect/provenance.json crates/trusty-mpm/src/assets/architect/provenance.json crates/trusty-mpm/src/bin/tm/commands/fleet/seed_history.tsv
  git commit -m "chore: merge main and regenerate the Architect bundle meta"
  git push

An edited Architect SKILL.md also needs its old sha256 added to
crates/trusty-mpm/src/core/project_skill_tier/architect_skills.rs.
If main moves again before merge, re-run this check after merging main again.

Bundle files this PR changes:
EOF
printf '  %s\n' "${hits[@]}"
exit 1
