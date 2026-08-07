#!/usr/bin/env bash
#
# check_public_docs.sh — public documentation allowlist gate (issue #5096).
#
# Why: the public website publishes a USER-FACING SUBSET of docs/, which holds
#   463 markdown files that are mostly internal — ADRs, behavior-contract specs,
#   research, plans, PRDs, presentations, design notes, runbooks. Publication is
#   one-way and search-indexed, so the boundary needs a gate for the same reason
#   the 500-SLOC cap did (scripts/check_line_cap.sh, #610): advice without a gate
#   loses. Two failure modes it closes, neither of which any human review
#   reliably catches:
#     1. a `docs/` file is renamed or deleted and the manifest keeps pointing at
#        it — the site then 404s, or (worse, depending on the loader) falls back
#        to something else;
#     2. someone widens the boundary by hand, in a diff that looks like an
#        ordinary docs edit, and an internal spec or ADR goes public.
#
# What: reads docs/public-manifest.tsv and fails on any of:
#     MISSING       a PAGE source that is not an existing file
#     ESCAPES-DOCS  a source outside docs/, absolute, or containing `..`
#     FORBIDDEN     a source inside a DO-NOT-PUBLISH tree, or named `*-internal.md`
#     NOT-MARKDOWN  a source that is not `.md`
#     DUP-ROUTE     two PAGE rows claiming the same route
#     DUP-SOURCE    two PAGE rows publishing the same source
#     BAD-ROUTE     a route not starting with `/`
#     ORPHAN-PAGE   a PAGE row before any SECTION row
#     DUP-SECTION   two SECTION rows with the same id
#     BAD-RECORD    a row whose first field is neither SECTION nor PAGE, or
#                   whose field count is wrong
#
#   FORBIDDEN is DEFENSE IN DEPTH, deliberately redundant with curation. The
#   manifest is hand-curated, so in a correct tree this check can never fire —
#   that is the point. It is what stops a later careless edit (a copy-pasted row,
#   a search-and-replace across the file, a well-meaning "add the specs index
#   too") from quietly moving the boundary. It is derived from the owner's tree
#   list, not from the manifest, so the manifest cannot grant itself an
#   exception.
#
#   The check is one-directional BY CONSTRUCTION: it validates that every listed
#   page is publishable. It does not, and must not, look for docs/ files that are
#   absent from the manifest — absence IS the default, and a "you forgot to list
#   this" warning would train authors to add rows. An unlisted page can never be
#   rendered because the site enumerates the manifest, never the tree.
#
# Usage:
#   bash scripts/check_public_docs.sh                    # default manifest
#   bash scripts/check_public_docs.sh --manifest <path>  # explicit (self-test)
#   bash scripts/check_public_docs.sh --root <path>      # resolve sources here
#   bash scripts/check_public_docs.sh --help
#
# Exit: 0 when every listed page resolves inside the public boundary; 1 on any
#   finding, with one `FAIL <CODE> line N: …` line per finding on stderr; 2 on a
#   usage error or an unreadable manifest.
#
# Test: scripts/check_public_docs_selftest.sh — fixture manifests under
#   scripts/test-data/public-docs/ cover each failure code plus the clean case,
#   including the two the issue names explicitly (a row pointing into
#   docs/specs/, and a row pointing at a file that does not exist).
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only — no associative arrays, no jq, no yq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MANIFEST=""
ROOT="$REPO_ROOT"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --manifest needs a path" >&2
        exit 2
      }
      MANIFEST="$2"
      shift 2
      ;;
    --root)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --root needs a path" >&2
        exit 2
      }
      ROOT="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,60p' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

[[ -n "$MANIFEST" ]] || MANIFEST="${ROOT}/docs/public-manifest.tsv"

if [[ ! -f "$MANIFEST" ]]; then
  echo "FAIL: manifest not found: ${MANIFEST}" >&2
  exit 2
fi

# The owner's DO-NOT-PUBLISH decision, as path prefixes. Kept here rather than
# read from the manifest so the manifest cannot exempt itself from it.
FORBIDDEN_TREES="docs/adr/
docs/specs/
docs/research/
docs/plans/
docs/prd/
docs/presentations/
docs/design/
docs/runbooks/"

# A per-crate tree may hold its own internal subdirectories mirroring the
# workspace-wide ones (docs/<crate>/research/, docs/<crate>/design/, ...). The
# owner's exclusion is by KIND, not by depth, so match those too.
FORBIDDEN_SEGMENTS="research
design
specs
spec
plans
prd
presentations
runbooks
decisions
sessions
regression-testing
_archive"

# A mixed-audience directory has no tree rule to lean on: docs/reference/ holds
# both public references and internal ones side by side, so the FIRST internal
# file split out of a published one (docs/reference/config-convention-internal.md,
# PR #5107) sits one row away from its public half with nothing structural
# separating them. `-internal.md` is the naming convention for that split, and
# this makes it mechanical: a source whose basename ends `-internal.md` can never
# be published, wherever it lives.
FORBIDDEN_SUFFIX="-internal.md"

fail=0
lineno=0
sections=""       # newline-separated section ids seen
current_section="" # section a PAGE row attaches to
routes=""         # newline-separated routes seen
sources=""        # newline-separated sources seen
page_count=0
section_count=0

report() {
  echo "FAIL $1 line $2: $3" >&2
  fail=1
}

# Why: `grep -qx` is the bash-3.2-safe membership test for a newline-separated
#   list; associative arrays are bash 4+.
# What: returns 0 when $2 appears as a whole line in $1.
contains_line() {
  printf '%s\n' "$1" | grep -qxF -- "$2"
}

while IFS= read -r raw || [[ -n "$raw" ]]; do
  lineno=$((lineno + 1))
  # Strip a trailing CR so a CRLF-committed manifest does not turn every title
  # into "Title\r" and every route into an unmatchable string.
  raw="${raw%$'\r'}"
  case "$raw" in
    '' | '#'*) continue ;;
  esac

  kind="${raw%%	*}"

  case "$kind" in
    SECTION)
      # SECTION <id> <title>
      rest="${raw#*	}"
      sid="${rest%%	*}"
      stitle="${rest#*	}"
      if [[ "$rest" == "$raw" || "$stitle" == "$rest" || -z "$sid" || -z "$stitle" ]]; then
        report BAD-RECORD "$lineno" "SECTION needs exactly 3 tab-separated fields: SECTION<TAB>id<TAB>title"
        continue
      fi
      if contains_line "$sections" "$sid"; then
        report DUP-SECTION "$lineno" "section id '${sid}' is already declared"
        continue
      fi
      sections="${sections}${sid}"$'\n'
      current_section="$sid"
      section_count=$((section_count + 1))
      ;;

    PAGE)
      # PAGE <section-id> <source> <route> <title>
      rest="${raw#*	}"
      psec="${rest%%	*}"
      rest2="${rest#*	}"
      src="${rest2%%	*}"
      rest3="${rest2#*	}"
      route="${rest3%%	*}"
      title="${rest3#*	}"
      if [[ "$rest" == "$raw" || "$rest2" == "$rest" || "$rest3" == "$rest2" ||
        "$title" == "$rest3" || -z "$psec" || -z "$src" || -z "$route" || -z "$title" ]]; then
        report BAD-RECORD "$lineno" "PAGE needs exactly 5 tab-separated fields: PAGE<TAB>section<TAB>source<TAB>route<TAB>title"
        continue
      fi
      page_count=$((page_count + 1))

      if [[ -z "$current_section" ]]; then
        report ORPHAN-PAGE "$lineno" "PAGE '${src}' appears before any SECTION row"
      elif ! contains_line "$sections" "$psec"; then
        report ORPHAN-PAGE "$lineno" "PAGE '${src}' names undeclared section '${psec}'"
      fi

      # Containment first: every later check assumes a docs/-relative path.
      # `..` is rejected outright rather than normalised, because a normalising
      # gate and a non-normalising site loader can disagree about where a path
      # lands, and the boundary must not depend on which one is right.
      case "$src" in
        /*)
          report ESCAPES-DOCS "$lineno" "source '${src}' is an absolute path"
          continue
          ;;
        *..*)
          report ESCAPES-DOCS "$lineno" "source '${src}' contains '..'"
          continue
          ;;
        docs/*) ;;
        *)
          report ESCAPES-DOCS "$lineno" "source '${src}' is outside docs/"
          continue
          ;;
      esac

      case "$src" in
        *.md) ;;
        *)
          report NOT-MARKDOWN "$lineno" "source '${src}' is not a .md file"
          continue
          ;;
      esac

      # Defense in depth — see the header. The `-internal.md` suffix first,
      # because it is the only rule that reaches into a mixed-audience
      # directory ...
      forbidden_hit=""
      case "$src" in
        *"$FORBIDDEN_SUFFIX") forbidden_hit="*${FORBIDDEN_SUFFIX}" ;;
      esac

      # ... then the top-level trees by prefix ...
      while [[ -z "$forbidden_hit" ]] && IFS= read -r tree; do
        [[ -z "$tree" ]] && continue
        case "$src" in
          "$tree"*)
            forbidden_hit="$tree"
            break
            ;;
        esac
      done <<<"$FORBIDDEN_TREES"

      # ... and finally the same kinds nested under a per-crate tree.
      if [[ -z "$forbidden_hit" ]]; then
        while IFS= read -r seg; do
          [[ -z "$seg" ]] && continue
          case "$src" in
            */"$seg"/*)
              forbidden_hit="*/${seg}/"
              break
              ;;
          esac
        done <<<"$FORBIDDEN_SEGMENTS"
      fi

      if [[ -n "$forbidden_hit" ]]; then
        report FORBIDDEN "$lineno" "source '${src}' matches the DO-NOT-PUBLISH rule '${forbidden_hit}'"
        continue
      fi

      if [[ ! -f "${ROOT}/${src}" ]]; then
        report MISSING "$lineno" "source '${src}' does not exist (renamed or deleted?)"
        continue
      fi

      case "$route" in
        /*) ;;
        *)
          report BAD-ROUTE "$lineno" "route '${route}' must start with '/'"
          ;;
      esac

      if contains_line "$routes" "$route"; then
        report DUP-ROUTE "$lineno" "route '${route}' is already claimed"
      else
        routes="${routes}${route}"$'\n'
      fi

      if contains_line "$sources" "$src"; then
        report DUP-SOURCE "$lineno" "source '${src}' is already published"
      else
        sources="${sources}${src}"$'\n'
      fi
      ;;

    *)
      report BAD-RECORD "$lineno" "unknown record type '${kind}' (expected SECTION or PAGE)"
      ;;
  esac
done <"$MANIFEST"

# Scan floor, same premise as check_changelog_fragment.sh's (#4618): "the
# manifest listed nothing" is indistinguishable from "the gate examined every
# page and found them all clean", and a gate that scans nothing is not a passing
# gate. An empty manifest means a bad path or a truncated file, never a healthy
# site with zero pages.
if [[ "$page_count" -lt 1 ]]; then
  echo "FAIL: SCAN FLOOR — ${MANIFEST} declares 0 page(s)." >&2
  echo "      Nothing was examined, so this gate could not have failed." >&2
  exit 1
fi

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<EOF

The public documentation manifest is an ALLOWLIST (${MANIFEST}).
Every PAGE row must name an existing .md file under docs/ that is outside the
DO-NOT-PUBLISH trees and not named \`*-internal.md\`, with a unique route. Fix the
rows above — never widen the boundary to make this gate green.
EOF
  exit 1
fi

echo "public-docs gate: ${page_count} page(s) across ${section_count} section(s) — all resolve inside the public boundary."
