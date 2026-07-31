#!/usr/bin/env bash
# scripts/assemble-changelog.sh
#
# Why: the per-PR changelog convention used to point every concurrent PR at the
# same lines of the same file — the topmost `## [Unreleased]` heading in
# crates/<crate>/CHANGELOG.md. That is a guaranteed textual conflict, not a
# hazard: on 2026-07-31 five concurrent trusty-mpm PRs (#4463, #4464, #4465,
# #4466, #4475) each added a bullet there and every merge forced the next PR to
# rebase and hand-resolve the section (#4399 burned three such rounds). Issue
# #4476 replaces it with per-PR FRAGMENT files whose names cannot collide, and
# this script is the release-time assembler that folds them into CHANGELOG.md.
#
# It also retires the mechanism behind #2793: nothing prepends a fresh
# `## [Unreleased]` section any more, so there is no duplicate-heading class of
# failure left to guard against. Fragments ARE the unreleased set; CHANGELOG.md
# carries only released version sections.
#
# What: reads every `crates/<crate-dir>/changelog.d/*.md` fragment, groups the
# bullets by category, and inserts one `## [<version>] — <YYYY-MM-DD>` section
# directly below CHANGELOG.md's `---` header separator (forward-only — existing
# history is never rewritten), then DELETES the consumed fragments in the same
# operation. Fails loudly rather than writing an empty or partial section.
#
# Fragment format (deliberately minimal):
#   line 1        a category token — Breaking | Added | Fixed | Performance |
#                 Changed | Removed | Security | Documentation
#                 (case-insensitive)
#   line 2+       the bullet(s), verbatim, `- ` at column 0; indented
#                 continuation/sub-bullet lines are preserved as authored
#
# Fragment naming: crates/<crate>/changelog.d/<issue-or-pr-number>-<slug>.md.
# The number makes the name collision-free across concurrent PRs (GitHub issue
# and PR numbers are unique per repo); the slug keeps two fragments for the same
# number distinct. Fragments are emitted in ascending numeric order within a
# category.
#
# Modes:
#   (default)   assemble into CHANGELOG.md and delete the consumed fragments
#   --check     validate only — no writes, no deletions (use in a release
#               pre-flight BEFORE anything else is mutated)
#   --stdout    render the section to stdout as `## [Unreleased]` for preview —
#               no writes, no deletions
#
# Idempotency: a successful default run leaves changelog.d/ holding only its
# tracked README.md placeholder, so a second run inserts nothing rather than an
# empty section. A run refuses outright when the target version heading is
# already present.
#
# An empty changelog.d/ is NOT an error — it is the steady state after a release,
# and a crate with no user-visible change since its last release has nothing to
# record. What guarantees a real change is recorded is the CI gate
# (scripts/check_changelog_fragment.sh), at the PR that makes the change.
#
# Test: `bash -n scripts/assemble-changelog.sh` for syntax. Functionally,
# `scripts/assemble-changelog.sh <crate-dir> --stdout` renders the pending
# fragments without touching any file, and `--check` exercises every validation
# branch (missing dir, unknown category, bodyless fragment, leftover
# `## [Unreleased]`, version already released).
#
# Usage:
#   scripts/assemble-changelog.sh <crate-dir> <version> [--check]
#   scripts/assemble-changelog.sh <crate-dir> --stdout
#
# Example:
#   scripts/assemble-changelog.sh trusty-mpm 1.3.2
#   scripts/assemble-changelog.sh trusty-mpm --stdout

set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Canonical category set and OUTPUT ORDER. cliff.toml's six groups (Breaking,
# Added, Fixed, Performance, Changed, Documentation) appear in cliff's own order
# so a fragment-assembled section is indistinguishable in shape from the
# git-cliff sections already in every crate's history. `Removed` and `Security`
# are added because the hand-written sections actually use them — a survey of
# every `## [Unreleased]` block in the workspace found 6 `### Removed` and 9
# `### Security` headings — and both are standard Keep a Changelog categories.
# Omitting them would have made the assembler reject real, already-written
# content.
CATEGORY_ORDER="Breaking Added Fixed Performance Changed Removed Security Documentation"

usage() {
  echo "Usage: scripts/assemble-changelog.sh <crate-dir> <version> [--check]" >&2
  echo "       scripts/assemble-changelog.sh <crate-dir> --stdout" >&2
  echo "" >&2
  echo "  <crate-dir>   directory under crates/ (e.g. trusty-mpm)" >&2
  echo "  <version>     released version for the new heading (e.g. 1.3.2)" >&2
  echo "  --check       validate fragments only; write nothing, delete nothing" >&2
  echo "  --stdout      preview the pending section as [Unreleased]; no writes" >&2
  exit 2
}

# Why: a fragment's category must be one of a fixed set, matched
# case-insensitively so `fixed`, `Fixed` and `FIXED` all work, and normalised to
# the canonical spelling used in the emitted heading.
# What: prints the canonical category for $1, or fails with a non-zero status.
canonical_category() {
  local raw="$1" want cat
  want="$(printf '%s' "${raw}" | tr '[:upper:]' '[:lower:]')"
  for cat in ${CATEGORY_ORDER}; do
    if [[ "${want}" == "$(printf '%s' "${cat}" | tr '[:upper:]' '[:lower:]')" ]]; then
      echo "${cat}"
      return 0
    fi
  done
  return 1
}

# Why: fragments must emit in a deterministic order so two people assembling the
# same set get byte-identical output.
# What: prints the fragment's leading issue/PR number, or 0 when the name has no
# numeric prefix (those sort first, then alphabetically by the caller's sort).
fragment_number() {
  local base="$1" num
  num="$(printf '%s' "${base}" | sed -E 's/^([0-9]+).*/\1/')"
  if [[ "${num}" =~ ^[0-9]+$ ]]; then echo "${num}"; else echo 0; fi
}

# Why: the whole point of failing loudly is that a malformed fragment must never
# degrade into a silently-dropped bullet.
# What: validates one fragment file and prints "<category>\t<path>" on success.
parse_fragment() {
  local path="$1" base first cat body
  base="$(basename "${path}")"

  first="$(sed -n '/[^[:space:]]/{p;q;}' "${path}" | tr -d '\r' | sed -E 's/^[[:space:]]+|[[:space:]]+$//g')"
  if [[ -z "${first}" ]]; then
    echo "ERROR: ${base} is empty — the first non-blank line must be a category" >&2
    echo "       (one of: ${CATEGORY_ORDER})." >&2
    return 1
  fi

  if ! cat="$(canonical_category "${first}")"; then
    echo "ERROR: ${base} has an unknown category '${first}'." >&2
    echo "       The first non-blank line must be one of: ${CATEGORY_ORDER}" >&2
    return 1
  fi

  body="$(fragment_body "${path}")"
  if ! printf '%s\n' "${body}" | grep -qE '^-[[:space:]]'; then
    echo "ERROR: ${base} has a category but no bullet — expected at least one" >&2
    echo "       line starting with '- ' after the category line." >&2
    return 1
  fi

  printf '%s\t%s\n' "${cat}" "${path}"
}

# Why: the bullet text is what a human wrote and reviewed; it is copied through
# verbatim (including indented sub-bullets) rather than reformatted. `\r` is the
# one exception — a fragment authored on a CRLF editor would otherwise carry its
# carriage returns into CHANGELOG.md, where they are invisible in review and
# corrupt the rendered bullet. The category line is cleaned the same way in
# parse_fragment().
# What: prints everything after the category line, with leading and trailing
# blank lines trimmed and any `\r` stripped.
fragment_body() {
  local path="$1"
  awk '
    !seen && /[^[:space:]]/ { seen = 1; next }   # drop the category line
    seen { print }
  ' "${path}" \
    | tr -d '\r' \
    | sed -e '/./,$!d' \
    | awk '{ lines[NR] = $0 } END { last = NR; while (last > 0 && lines[last] ~ /^[[:space:]]*$/) last--; for (i = 1; i <= last; i++) print lines[i] }'
}

# Why: keep rendering in one place so --stdout preview and the real write can
# never diverge.
# What: prints the assembled section (heading + grouped bullets) for the given
# heading text, reading the tab-separated "<category>\t<path>" list on stdin.
render_section() {
  local heading="$1" parsed="$2" cat path first_group=1
  printf '%s\n' "${heading}"
  for cat in ${CATEGORY_ORDER}; do
    local group
    group="$(printf '%s\n' "${parsed}" | awk -F'\t' -v c="${cat}" '$1 == c { print $2 }')"
    [[ -z "${group}" ]] && continue
    printf '\n### %s\n\n' "${cat}"
    first_group=0
    while IFS= read -r path; do
      [[ -z "${path}" ]] && continue
      fragment_body "${path}"
    done <<<"${group}"
  done
  if [[ "${first_group}" -eq 1 ]]; then
    echo "ERROR: no category groups rendered — refusing to write an empty section." >&2
    return 1
  fi
}

main() {
  [[ $# -lt 2 || $# -gt 3 ]] && usage

  local crate_dir="$1" version="$2" mode="${3:-write}"

  if [[ "${version}" == "--stdout" ]]; then
    [[ $# -ne 2 ]] && usage
    mode="stdout"
    version=""
  else
    case "${mode}" in
      write | --check) ;;
      *) usage ;;
    esac
    if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
      echo "ERROR: '${version}' does not look like a version (expected X.Y.Z)" >&2
      usage
    fi
  fi

  local crate_path="${WORKSPACE_ROOT}/crates/${crate_dir}"
  if [[ ! -d "${crate_path}" ]]; then
    echo "ERROR: crate directory not found: crates/${crate_dir}" >&2
    exit 1
  fi

  local changelog="${crate_path}/CHANGELOG.md"
  if [[ ! -f "${changelog}" ]]; then
    echo "ERROR: crates/${crate_dir}/CHANGELOG.md not found" >&2
    exit 1
  fi

  local frag_dir="${crate_path}/changelog.d"

  # A fragment is a `.md` file DIRECTLY in changelog.d/. Anything deeper is
  # rejected rather than skipped: a fragment at changelog.d/sub/12-x.md looks
  # authored and passes the CI gate, but a depth-limited collector would drop it
  # from the release with no diagnostic. Silent omission of a reviewed bullet is
  # the worst failure this script can have, so it is a hard error.
  if [[ -d "${frag_dir}" ]]; then
    local nested
    nested="$(find "${frag_dir}" -mindepth 2 -type f -name '*.md' | LC_ALL=C sort)"
    if [[ -n "${nested}" ]]; then
      echo "ERROR: changelog fragments must sit directly in crates/${crate_dir}/changelog.d/;" >&2
      echo "       these are nested and would never be assembled:" >&2
      printf '%s\n' "${nested}" | sed 's/^/         /' >&2
      echo "       Move them up one level. Nothing has been modified." >&2
      exit 1
    fi
  fi

  # Collect fragments. `find` (not a glob) so a missing directory is a clean
  # empty result rather than a literal unexpanded pattern. README.md is the
  # tracked placeholder that keeps changelog.d/ present between releases (see
  # the trailing note in this script) and is never a fragment.
  local fragments=()
  if [[ -d "${frag_dir}" ]]; then
    while IFS= read -r f; do
      [[ -z "${f}" ]] && continue
      [[ "$(basename "${f}")" == "README.md" ]] && continue
      fragments+=("${f}")
    done < <(find "${frag_dir}" -maxdepth 1 -type f -name '*.md' | LC_ALL=C sort)
  fi

  # A leftover `## [Unreleased]` heading means hand-written bullets are sitting in
  # CHANGELOG.md instead of in changelog.d/. Assembling around them would ship a
  # released section that silently omits them while a stale [Unreleased] hangs
  # above it — the exact failure the old #2793 stopgap in bump-version.sh existed
  # to catch, caught here instead and BEFORE any mutation. Checked independently
  # of the fragment count, because a crate with no fragments and a populated
  # [Unreleased] section is the worst case: a release that records nothing while
  # real entries sit one heading away.
  if [[ "${mode}" != "stdout" ]] && grep -qE '^## \[Unreleased\]' "${changelog}"; then
    echo "ERROR: ${changelog} still has a '## [Unreleased]' heading." >&2
    echo "       Fragments are the source of truth for the unreleased set now" >&2
    echo "       (issue #4476), so CHANGELOG.md must carry released sections only." >&2
    echo "       Fold those bullets into crates/${crate_dir}/changelog.d/ fragments" >&2
    echo "       (or into the section you are about to cut), remove the heading," >&2
    echo "       then re-run. Nothing has been modified." >&2
    exit 1
  fi

  # No fragments is a legitimate, and in fact the STEADY, state: a successful
  # release consumes every fragment, so the next release of a crate that has had
  # no user-visible change since has nothing pending. Treating that as an error
  # made the mechanism switch itself off after every release — the crate became
  # unreleasable until somebody invented a fragment for it. Report and insert
  # nothing. What guarantees a real change IS recorded is the CI gate
  # (scripts/check_changelog_fragment.sh), which refuses a PR that touches
  # crates/<crate>/src/** without a fragment — enforcement belongs at the commit
  # that makes the change, not at the release that ships it.
  if [[ "${#fragments[@]}" -eq 0 ]]; then
    echo "NOTE: no changelog fragments pending in crates/${crate_dir}/changelog.d/;" >&2
    echo "      no release section inserted into crates/${crate_dir}/CHANGELOG.md." >&2
    return 0
  fi

  # Validate every fragment BEFORE emitting or deleting anything, and sort into
  # (category, ascending number, name) order.
  local parsed="" f line
  for f in "${fragments[@]}"; do
    line="$(parse_fragment "${f}")" || exit 1
    parsed+="$(printf '%s\t%s' "$(fragment_number "$(basename "${f}")")" "${line}")"$'\n'
  done
  # parsed rows are "<number>\t<category>\t<path>"; sort by number then path,
  # then drop the sort key so render_section sees "<category>\t<path>".
  parsed="$(printf '%s' "${parsed}" | LC_ALL=C sort -t$'\t' -k1,1n -k3,3 | cut -f2-)"

  if [[ "${mode}" == "stdout" ]]; then
    render_section "## [Unreleased]" "${parsed}"
    return 0
  fi

  if grep -qE "^## \[${version//./\\.}\]" "${changelog}"; then
    echo "ERROR: ${changelog} already has a '## [${version}]' section." >&2
    echo "       Refusing to insert a second one. Nothing has been modified." >&2
    exit 1
  fi

  local sep_line
  sep_line="$(grep -n -m1 '^---[[:space:]]*$' "${changelog}" | cut -d: -f1 || true)"
  if [[ -z "${sep_line}" ]]; then
    echo "ERROR: ${changelog} has no '---' header separator — cannot find the" >&2
    echo "       insertion point. Nothing has been modified." >&2
    exit 1
  fi

  local section
  section="$(render_section "## [${version}] — $(date -u +%Y-%m-%d)" "${parsed}")" || exit 1

  if [[ "${mode}" == "--check" ]]; then
    echo "OK: ${#fragments[@]} fragment(s) in crates/${crate_dir}/changelog.d/ are valid;" >&2
    echo "    crates/${crate_dir}/CHANGELOG.md is ready for a [${version}] section." >&2
    return 0
  fi

  # Write: header lines through the separator, the new section, then the rest of
  # the file with its leading blank lines normalised to exactly one.
  local tmp="${changelog}.assemble.$$"
  # shellcheck disable=SC2064  # expand tmp now so the trap targets this exact file
  trap "rm -f '${tmp}'" EXIT
  {
    sed -n "1,${sep_line}p" "${changelog}"
    echo ""
    printf '%s\n' "${section}"
    echo ""
    sed -n "$((sep_line + 1)),\$p" "${changelog}" | sed -e '/./,$!d'
  } >"${tmp}"
  mv "${tmp}" "${changelog}"
  trap - EXIT

  # Delete the consumed fragments in the SAME operation, so a half-applied
  # release (section written, fragments still pending) is not representable.
  #
  # The DIRECTORY stays, kept alive by its tracked README.md. Removing it would
  # switch the mechanism off: BASE-AGENT.md tells agents to use fragments
  # "whenever the project has a changelog.d/ directory", so a post-release tree
  # with no such directory sends the next PR straight back to editing
  # `## [Unreleased]` — which is both the conflict this change removes and, one
  # release later, a hard stop in the leftover-[Unreleased] check above.
  rm -f "${fragments[@]}"

  echo "Assembled ${#fragments[@]} fragment(s) into crates/${crate_dir}/CHANGELOG.md as [${version}]" >&2
  echo "and removed them from crates/${crate_dir}/changelog.d/." >&2
}

main "$@"
