#!/usr/bin/env bash
#
# check-pr-version-bump.sh — pre-merge half of the version-parity gate
# (issue #4421; original defect class #3366).
#
# Why: `scripts/check-version-parity.sh` runs on PUSH TO MAIN ONLY, so drift is
#   only ever caught AFTER it lands. A crate sails through review fully green
#   and main goes red minutes later. It recurred TWICE on 2026-07-30 alone
#   (`trusty-installer 0.4.10` at 6078b21c, nine consecutive red runs;
#   `trusty-agents-common 0.3.0` at e8f30ef0, hours after the first fix). #3366
#   proposed a merge-time guard and was closed without one being written, which
#   is why the hole kept producing incidents.
#
#   #4421 offered three options. This implements the SECOND — the lighter
#   PR-time check — and the post-merge tarball diff stays exactly as it is:
#
#     1. Run the full parity check on PRs. Rejected: it downloads and diffs the
#        published tarball for every publishable crate against the PR's tree,
#        so a PR that touches nothing publishable still pays the whole cost,
#        and it fails CLOSED on registry trouble — one crates.io hiccup would
#        block every open PR. It also mis-fires on a merge train, where crate
#        N's parity depends on crate N-1 having landed.
#     2. (chosen) Fail when a PR changes `crates/<X>/src/**` without bumping
#        `crates/<X>/Cargo.toml`'s version AND that version is already live on
#        crates.io. This is exactly the shape of both 07-30 recurrences,
#        needs one sparse-index GET per touched crate (no tarballs), is
#        deterministic from the diff, and answers a question that is TRUE of
#        the PR itself rather than of a moving main.
#     3. Keep it post-merge but louder. Rejected as the primary remedy: main
#        still goes red, which is the cost #4421 is about. (The louder half
#        landed anyway — see ci.yml's notify job, #4179.)
#
# What: for every crate whose `crates/<X>/src/**` changed between the merge
#   base and HEAD:
#     - crate absent at HEAD (deleted by the PR)  -> SKIP
#     - `publish = false`                          -> SKIP
#     - version differs from the merge base        -> PASS (bumped)
#     - version unchanged, not live on crates.io   -> PASS (nothing to drift from)
#     - version unchanged, ALREADY live            -> FAIL (the #3366 defect)
#     - crates.io unreachable                      -> WARN, does not fail
#
#   The unreachable case deliberately does NOT fail. This gate is a pre-merge
#   EARLY WARNING; `scripts/check-version-parity.sh` on main remains the
#   fail-closed authority, so a registry outage costs late detection (today's
#   status quo) instead of blocking every PR in the repo.
#
# Usage:
#   PR_VERSION_BUMP_BASE=<ref> bash scripts/check-pr-version-bump.sh
#
# Exit: 0 when every touched crate is clear (or only warnings were raised);
#   1 when at least one crate changed source under an already-published version,
#   or when the diff resolved to zero paths at all (the #4618 scan floor — see
#   the inline note in `main`).
#
# Test: scripts/check-ci-helpers-selftest.sh (`check-pr-version-bump:` cases)
#   exercises the crate-extraction and version-decision logic against synthetic
#   diffs with the registry lookup stubbed; the live crates.io path is covered
#   by running this script against a real branch (see the #4421 PR body).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

BASE="${PR_VERSION_BUMP_BASE:-origin/main}"

# manifest_field <file> <field> — first literal `field = "value"` in [package].
manifest_field() {
  sed -n '/^\[package\]/,/^\[[^p]/p' "$1" 2>/dev/null |
    grep -m1 -E "^${2}[[:space:]]*=[[:space:]]*\"" |
    sed -E 's/^[^"]*"([^"]*)".*/\1/'
}

# manifest_is_unpublishable <file> — true when [package] carries publish = false.
manifest_is_unpublishable() {
  sed -n '/^\[package\]/,/^\[[^p]/p' "$1" 2>/dev/null |
    grep -qE '^publish[[:space:]]*=[[:space:]]*false'
}

# sparse_index_path <crate-name> — crates.io sparse-index route for a name.
sparse_index_path() {
  local name="$1"
  local n=${#name}
  case "$n" in
    1) echo "1/${name}" ;;
    2) echo "2/${name}" ;;
    3) echo "3/${name:0:1}/${name}" ;;
    *) echo "${name:0:2}/${name:2:2}/${name}" ;;
  esac
}

# is_version_published <crate-name> <version>
#   0 = published, 1 = not published, 2 = could not determine.
is_version_published() {
  local name="$1" version="$2"
  # Allow the selftest to substitute a deterministic oracle for the network.
  if [ -n "${PR_VERSION_BUMP_REGISTRY_STUB:-}" ]; then
    "${PR_VERSION_BUMP_REGISTRY_STUB}" "$name" "$version"
    return $?
  fi

  local url="https://index.crates.io/$(sparse_index_path "$name")"
  local body status
  body="$(curl -sS --max-time 20 --retry 2 --retry-delay 2 -w '\n%{http_code}' "$url" 2>/dev/null)" || return 2
  status="${body##*$'\n'}"
  body="${body%$'\n'*}"

  case "$status" in
    404) return 1 ;;
    200) ;;
    *) return 2 ;;
  esac

  if grep -qF "\"vers\":\"${version}\"" <<<"$body"; then
    return 0
  fi
  return 1
}

main() {
  local merge_base
  if ! merge_base="$(git merge-base "${BASE}" HEAD 2>/dev/null)"; then
    echo "check-pr-version-bump: cannot resolve merge-base against '${BASE}'" >&2
    return 1
  fi
  echo "Merge base: ${merge_base}"

  local changed changed_count touched
  changed="$(git diff --name-only --no-renames "${merge_base}" HEAD)"

  # #4618 scan floor. Two very different situations both leave `touched` empty,
  # and only ONE of them is a pass:
  #   - the diff RESOLVED and simply contains no crate source (a docs-only or
  #     CI-only PR) — nothing can drift, so passing is correct;
  #   - the diff resolved to NOTHING AT ALL, which no real PR does. That means
  #     something upstream broke — a wrong base ref, a shallow clone with no
  #     merge base, a detached HEAD — and the gate would report OK having
  #     examined zero paths.
  # Asserting a non-zero overall path count before the legitimate-empty branch
  # below separates them, so an unresolvable base fails loudly instead of
  # reading as a clean bill of health. Same floor and wording as
  # scripts/check_changelog_fragment.sh, which faces the identical hazard.
  changed_count="$(printf '%s' "${changed}" | grep -c . || true)"
  if [ "${changed_count:-0}" -eq 0 ]; then
    echo "FAIL: SCAN FLOOR — the diff ${merge_base}..HEAD lists 0 changed path(s)." >&2
    echo "      Nothing was examined, so this gate could not have failed. Check that" >&2
    echo "      '${BASE}' is the right base and that CI checked out with fetch-depth: 0." >&2
    echo "      A gate that scans nothing is not a passing gate (issue #4618)." >&2
    return 1
  fi

  touched="$(
    grep -E '^crates/[^/]+/src/' <<<"${changed}" |
      cut -d/ -f2 |
      sort -u
  )" || true

  if [ -z "${touched}" ]; then
    echo "[ OK ] ${changed_count} changed path(s) examined; none under crates/<crate>/src/**"
    return 0
  fi

  local failures=0 warnings=0 crate manifest name version base_version
  while IFS= read -r crate; do
    [ -n "${crate}" ] || continue
    manifest="crates/${crate}/Cargo.toml"

    if [ ! -f "${manifest}" ]; then
      echo "[SKIP] ${crate} — crate removed by this PR"
      continue
    fi
    if manifest_is_unpublishable "${manifest}"; then
      echo "[SKIP] ${crate} — publish = false"
      continue
    fi

    name="$(manifest_field "${manifest}" name)"
    version="$(manifest_field "${manifest}" version)"
    if [ -z "${name}" ] || [ -z "${version}" ]; then
      echo "[WARN] ${crate} — could not read name/version from ${manifest}"
      warnings=$((warnings + 1))
      continue
    fi

    base_version="$(
      git show "${merge_base}:${manifest}" 2>/dev/null |
        sed -n '/^\[package\]/,/^\[[^p]/p' |
        grep -m1 -E '^version[[:space:]]*=[[:space:]]*"' |
        sed -E 's/^[^"]*"([^"]*)".*/\1/'
    )" || true

    if [ -z "${base_version}" ]; then
      echo "[ OK ] ${name} — new crate on this branch, nothing published to drift from"
      continue
    fi
    if [ "${version}" != "${base_version}" ]; then
      echo "[ OK ] ${name} — version bumped ${base_version} -> ${version}"
      continue
    fi

    set +e
    is_version_published "${name}" "${version}"
    local rc=$?
    set -e

    case "${rc}" in
      1)
        echo "[ OK ] ${name} ${version} — src changed, version not yet published"
        ;;
      0)
        echo "[FAIL] ${name} ${version} — src/** changed under a version that is ALREADY"
        echo "       published on crates.io, and Cargo.toml was not bumped. Merging this"
        echo "       turns main's 'Version parity' workflow red (issues #4421, #3366)."
        echo "       Fix: bump ${manifest}'s version (patch/minor/major per the change)."
        failures=$((failures + 1))
        ;;
      *)
        echo "[WARN] ${name} ${version} — crates.io lookup failed; cannot tell whether this"
        echo "       version is published. Not failing the PR: the post-merge parity check"
        echo "       (scripts/check-version-parity.sh) still fails closed on main."
        warnings=$((warnings + 1))
        ;;
    esac
  done <<<"${touched}"

  echo
  if [ "${failures}" -gt 0 ]; then
    echo "check-pr-version-bump: ${failures} crate(s) would drift on merge."
    return 1
  fi
  echo "check-pr-version-bump: clear (${warnings} warning(s))."
  return 0
}

main "$@"
