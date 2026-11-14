#!/usr/bin/env bash
#
# check_bundle_pr_current.sh — an Architect-bundle PR must hold every bundle
# change main has (issue #11120; owner ruling 2026-11-14, "bundle PRs must be
# current with main").
#
# Why: a bundle PR regenerates python/trusty-architect/provenance.json, the
#   asset copy of it and seed_history.tsv. Two bundle PRs cut from one base are
#   each green alone; merged one after the other, git combines their generated
#   files into bytes neither generated, and main goes red (#11236 + #11191,
#   repaired by #11246). A commit on main that touches no bundle file cannot
#   cause that, so lacking one is no reason to take main again (#11248 fell
#   behind on a TICKETING.md-only commit).
# What: lists the files the PR changes, then
#   - PASS when none is an Architect bundle path (BUNDLE_PATHS below);
#   - else PASS when <head> contains <base>'s tip;
#   - else walks every commit reachable from <base> but not from <head>
#     (`git log <head>..<base>`), one commit at a time, and FAILS BEHIND, with
#     those commits, the bundle files they change and the regeneration
#     commands, when any of them changes a bundle path; PASS, naming how many
#     commits behind, when none does.
#   The walk is per commit, not the net diff merge-base..<base>: a bundle file
#   that main changed and then reverted still FAILS. provenance.json is a pure
#   function of its inputs, but seed_history.tsv is append-only
#   (architect_bundle_meta.py history keeps every line it has), so a reverted
#   seed asset still leaves history lines behind; and the walk needs no choice
#   among several merge bases. A merge commit counts only for a file it changes
#   against EVERY parent (`-c`); a change it brings from one side is that
#   side's own commit, walked on its own when <head> lacks it.
#   The changed files come from --files (one path per line, `-` for stdin),
#   from --range (`git diff --no-renames`), or by default from
#   `git diff --no-renames <merge-base> <head>`, the PR's own changes, taken
#   over every merge base when there are several. --no-renames
#   lists both sides of a rename, on either side, so a rename into or out of
#   the bundle counts; a deletion lists the deleted path. Paths are read with
#   -z, so a path git would C-quote (non-ASCII, a newline) is matched as its
#   real bytes.
#   --fetch <remote> is the CI mode: --base is then a branch name. The script
#   fetches that branch into refs/remotes/<remote>/<base>, fetches the head
#   SHA when it is absent, and verifies each revision on its own.
#   Fails closed: a failed fetch, an unresolvable head or base, a shallow
#   repository, no merge base, a failed git command or a failed temp file
#   exits 2, never 0.
#   Not guaranteed: it judges main as fetched when it runs, and re-runs only
#   when the PR changes or a run is re-requested. A bundle PR that passed can
#   be overtaken by another bundle PR merging first; until a merge queue or a
#   re-run on every main move exists, merge order (the supervisor) covers that.
# Test: scripts/check_bundle_pr_current_selftest.sh
#
# Usage: check_bundle_pr_current.sh [--repo DIR] [--fetch <remote>]
#          --head <sha> --base <ref> [--files <file|-> | --range <a..b>]
# Exit: 0 pass, 1 behind a bundle change on main, 2 usage or git error.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git only.

set -euo pipefail
# An unexpected failure is an infrastructure error, never a verdict (#11120).
trap 'echo "ERROR: unexpected failure at line $LINENO" >&2; exit 2' ERR

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
# How many of main's bundle commits and files a FAIL lists.
SHOW_MAX=10

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
die() { echo "ERROR: $*" >&2; exit 2; }

if [ -n "$REMOTE" ]; then
  # CI mode: --head must be a full SHA, so it can never parse as a git option.
  case "$HEAD_REV" in
    *[!0-9a-f]*) die "--fetch needs --head as a hex SHA, got '$HEAD_REV'" ;;
  esac
  [ "${#HEAD_REV}" -ge 40 ] || die "--fetch needs --head as a full SHA, got '$HEAD_REV'"
  g fetch --no-tags --quiet "$REMOTE" "+refs/heads/${BASE_REV}:refs/remotes/${REMOTE}/${BASE_REV}" \
    || die "cannot fetch branch '$BASE_REV' from '$REMOTE'"
  if ! g cat-file -e "${HEAD_REV}^{commit}" 2>/dev/null; then
    g fetch --no-tags --quiet "$REMOTE" "$HEAD_REV" \
      || die "cannot fetch head '$HEAD_REV' from '$REMOTE'"
  fi
  BASE_REV="refs/remotes/${REMOTE}/${BASE_REV}"
fi

head_sha="$(g rev-parse --verify --quiet "${HEAD_REV}^{commit}")" \
  || die "head '$HEAD_REV' does not resolve to a commit"
base_sha="$(g rev-parse --verify --quiet "${BASE_REV}^{commit}")" \
  || die "base '$BASE_REV' does not resolve to a commit"
# A shallow history can hide the merge base and the commits between it and
# either tip, so every answer below could be wrong in the passing direction.
shallow="$(g rev-parse --is-shallow-repository)" || die "cannot tell whether the repository is shallow"
[ "$shallow" = "false" ] || die "the repository is shallow; fetch full history (fetch-depth: 0)"

# Paths are kept NUL-delimited in temp files: command substitution drops NUL
# bytes, and -z is what keeps git from C-quoting a path.
tmpd="$(mktemp -d)" || die "cannot create a temp directory"
trap 'rm -rf "$tmpd"' EXIT
changed="${tmpd}/changed"
if [ -n "$FILES" ]; then
  if [ "$FILES" = "-" ]; then
    tr '\n' '\0' > "$changed" || die "cannot read --files from stdin"
  else
    [ -r "$FILES" ] || die "cannot read --files '$FILES'"
    tr '\n' '\0' < "$FILES" > "$changed" || die "cannot read --files '$FILES'"
  fi
elif [ -n "$RANGE" ]; then
  g diff -z --no-renames --name-only "$RANGE" > "$changed" || die "git diff $RANGE failed"
else
  # <base>...<head> against EVERY merge base: `git diff A...B` picks one of
  # several, and a criss-cross merge can hide the PR's own bundle edit from it.
  g merge-base --all "$base_sha" "$head_sha" > "${tmpd}/bases" \
    || die "head ${head_sha} and ${BASE_REV} ${base_sha} share no history"
  : > "$changed"
  while IFS= read -r mb; do
    g diff -z --no-renames --name-only "$mb" "$head_sha" >> "$changed" \
      || die "git diff ${mb} ${head_sha} failed"
  done < "${tmpd}/bases"
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
  is_bundle "$f" || continue
  seen=0
  for h in ${hits[@]+"${hits[@]}"}; do
    if [ "$h" = "$f" ]; then seen=1; break; fi
  done
  [ "$seen" -eq 1 ] || hits+=("$f")
done < "$changed"

if [ "${#hits[@]}" -eq 0 ]; then
  echo "PASS: no Architect bundle file changed; this PR need not be current with ${BASE_REV}."
  exit 0
fi

rc=0
g merge-base --is-ancestor "$base_sha" "$head_sha" || rc=$?
case "$rc" in
  0) echo "PASS: ${#hits[@]} Architect bundle file(s) changed and head ${head_sha} contains ${BASE_REV} ${base_sha}."
     exit 0 ;;
  1) ;;
  *) die "git merge-base --is-ancestor failed (exit ${rc})" ;;
esac
g merge-base "$base_sha" "$head_sha" > /dev/null \
  || die "head ${head_sha} and ${BASE_REV} ${base_sha} share no history"
behind="$(g rev-list --count "${head_sha}..${base_sha}")" || die "git rev-list ${head_sha}..${base_sha} failed"

# One record per commit main has and the head lacks: "\001<short> <subject>"
# then its changed paths, all NUL-terminated. The pathspec only narrows the
# walk; is_bundle below decides. --full-history keeps every commit that
# touches a pathspec, -c gives a merge only the files it changes against
# every parent.
log="${tmpd}/log"
g --literal-pathspecs log -z -c --no-renames --full-history --name-only \
  --format='%x01%h %s' "${head_sha}..${base_sha}" -- "${BUNDLE_PATHS[@]}" > "$log" \
  || die "git log ${head_sha}..${base_sha} failed"

mfiles=()
mcommits=()
cur=""
counted=1
while IFS= read -r -d '' tok || [ -n "$tok" ]; do
  case "$tok" in
    $'\001'*) cur="${tok#?}"; counted=0; continue ;;
  esac
  tok="${tok#$'\n'}"
  [ -n "$tok" ] || continue
  is_bundle "$tok" || continue
  if [ "$counted" -eq 0 ]; then mcommits+=("$cur"); counted=1; fi
  seen=0
  for f in ${mfiles[@]+"${mfiles[@]}"}; do
    if [ "$f" = "$tok" ]; then seen=1; break; fi
  done
  [ "$seen" -eq 1 ] || mfiles+=("$tok")
done < "$log"

if [ "${#mfiles[@]}" -eq 0 ]; then
  echo "PASS: ${#hits[@]} Architect bundle file(s) changed; head ${head_sha} is ${behind} commit(s) behind ${BASE_REV} ${base_sha}, and none of those commits changes an Architect bundle file."
  exit 0
fi

# show_capped <item>...: one indented line per item, at most SHOW_MAX.
show_capped() {
  local n=0 item
  for item in "$@"; do
    n=$((n + 1))
    [ "$n" -le "$SHOW_MAX" ] || { echo "  ... and $(($# - SHOW_MAX)) more"; return 0; }
    echo "  ${item}"
  done
}

cat <<EOF
FAIL BEHIND: this PR changes ${#hits[@]} Architect bundle file(s), and ${#mcommits[@]} of the
${behind} commit(s) on ${BASE_REV} ${base_sha} that head ${head_sha}
lacks change Architect bundle files.

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
If another bundle commit lands on main before merge, re-run this check after
merging main again.

Bundle files changed on ${BASE_REV} that this head lacks:
EOF
show_capped "${mfiles[@]}"
echo "Commits on ${BASE_REV} that change them:"
show_capped "${mcommits[@]}"
echo "Bundle files this PR changes:"
printf '  %s\n' "${hits[@]}"
exit 1
