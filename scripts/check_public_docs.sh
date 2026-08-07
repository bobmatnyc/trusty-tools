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
#   With `--stale` it additionally reads docs/public-stale-terms.tsv and fails
#   on:
#     STALE         a published page contains a retired name or anti-pattern
#     BAD-WAIVER    a waiver row is malformed, unexplained, dead, or off-count
#     BAD-TERM      a TERM row is malformed, or its ERE cannot be built
#
#   STALE closes the gap the existence checks leave open: they prove a published
#   page IS THERE, never that what it says is still true. Four published pages
#   were caught naming binaries and crates that no longer exist, each found by a
#   human reading closely. A reader who copies `cargo install trusty-mpmd` off
#   the public site gets an error, not a daemon.
#
#   It is OPT-IN and wired to nothing (issue #5125). The pre-commit hook and CI
#   run this script with no arguments, which skips the pass entirely, because 10
#   of the 27 published pages hit today and their repairs are in flight on other
#   branches. Turning it on is a one-line change once those land; the self-test
#   proves the logic against fixtures in the meantime.
#
#   Scope is the published set — the sources of PAGE rows that passed every
#   other check. Excluded trees may hold historical names, and often must.
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
#   bash scripts/check_public_docs.sh                       # default manifest
#   bash scripts/check_public_docs.sh --manifest <path>     # explicit (self-test)
#   bash scripts/check_public_docs.sh --root <path>         # resolve sources here
#   bash scripts/check_public_docs.sh --stale               # add the STALE pass
#   bash scripts/check_public_docs.sh --stale-terms <path>  # implies --stale
#   bash scripts/check_public_docs.sh --help
#
# Exit: 0 when every listed page resolves inside the public boundary; 1 on any
#   finding, with one `FAIL <CODE> line N: …` line per finding on stderr; 2 on a
#   usage error or an unreadable manifest.
#
#   A STALE finding's `line N` is the MANIFEST line that published the page; the
#   page's own line number is in the message. A BAD-WAIVER or BAD-TERM finding's
#   `line N` is the line in the stale-terms file, which the message names.
#
# Test: scripts/check_public_docs_selftest.sh — fixture manifests under
#   scripts/test-data/public-docs/ cover each failure code plus the clean case,
#   including the two the issue names explicitly (a row pointing into
#   docs/specs/, and a row pointing at a file that does not exist). The STALE
#   cases run against scripts/test-data/public-docs/fakeroot/ via --root, so
#   they never depend on what the real docs/ tree happens to say today.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only — no associative arrays, no jq, no yq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MANIFEST=""
ROOT="$REPO_ROOT"
STALE_MODE=0
STALE_TERMS=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --stale)
      STALE_MODE=1
      shift
      ;;
    --stale-terms)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --stale-terms needs a path" >&2
        exit 2
      }
      # Implies --stale: a flag that names a terms file and then silently
      # searches for none of them is a trap, not a convenience.
      STALE_TERMS="$2"
      STALE_MODE=1
      shift 2
      ;;
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
      sed -n '2,90p' "$0" >&2
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
#
# The convention is documented for AUTHORS in docs/public-manifest.tsv's header,
# under "NAMING AN INTERNAL DOC" — a rule that only exists inside the gate that
# enforces it is a trap, however right the rule is.
FORBIDDEN_SUFFIX="-internal.md"

fail=0
lineno=0
sections=""       # newline-separated section ids seen
current_section="" # section a PAGE row attaches to
routes=""         # newline-separated routes seen
sources=""        # newline-separated sources seen
published=""      # "<manifest-line><TAB><source>" per page that passed everything
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

# ---------------------------------------------------------------------------
# STALE pass (--stale). See the header.
# ---------------------------------------------------------------------------

STALE_WAIVERS=""   # "<terms-line><TAB><source><TAB><ere><TAB><count><TAB><reason>"
STALE_TERM_LIST="" # "<declared-ere><TAB><expanded-ere><TAB><remedy>"
STALE_TERM_COUNT=0

# Why: the former-repo clone URLs are already written down once, in
#   docs/reference/former-repos.md. Hand-copying them into a second list is how
#   two lists drift, so this term is DERIVED from that page and the two cannot
#   disagree. A derivation that finds nothing is treated as a failure, not as an
#   empty term: a pattern matching nothing looks exactly like a clean tree.
# What: prints `github\.com/bobmatnyc/(a|b|…)` from the `bobmatnyc/<repo>` names
#   in that page. Returns 1 when the page is unreadable or names no repos.
# Test: scripts/check_public_docs_selftest.sh — the fakeroot carries its own
#   minimal former-repos.md, so the STALE fixtures exercise this path.
derive_former_repo_urls() {
  local page="${ROOT}/docs/reference/former-repos.md"
  local names
  [[ -f "$page" ]] || return 1
  # shellcheck disable=SC2016  # the backticks are literal grep pattern text
  names="$(grep -oE '`bobmatnyc/[a-z0-9-]+`' "$page" |
    tr -d '`' | sed 's|^bobmatnyc/||' | sort -u | paste -sd'|' -)" || return 1
  [[ -n "$names" ]] || return 1
  printf 'github\\.com/bobmatnyc/(%s)' "$names"
}

# Returns 0 if $1 is a POSIX ERE grep can compile. Matching nothing is fine;
# only a compile error (grep exit > 1) is a finding.
ere_compiles() {
  local rc=0
  printf '' | grep -qE -- "$1" >/dev/null 2>&1 || rc=$?
  [[ "$rc" -le 1 ]]
}

# Prints the waived line-count for source $1 / declared-ERE $2, or 0.
waiver_count() {
  local wl wsrc were wcount wreason out=0
  while IFS=$'\t' read -r wl wsrc were wcount wreason; do
    [[ -n "$wl" ]] || continue
    if [[ "$wsrc" == "$1" && "$were" == "$2" ]]; then
      out="$wcount"
      break
    fi
  done <<<"$STALE_WAIVERS"
  printf '%s' "$out"
}

# Prints the stale-terms line number of the waiver for source $1 / ERE $2, or 0.
waiver_line() {
  local wl wsrc were wcount wreason out=0
  while IFS=$'\t' read -r wl wsrc were wcount wreason; do
    [[ -n "$wl" ]] || continue
    if [[ "$wsrc" == "$1" && "$were" == "$2" ]]; then
      out="$wl"
      break
    fi
  done <<<"$STALE_WAIVERS"
  printf '%s' "$out"
}

# Reads $STALE_TERMS into $STALE_TERM_LIST / $STALE_WAIVERS, validating both
# record types. Called directly, never in `$(…)`: a subshell would discard every
# global it sets, `fail` included.
stale_load_terms() {
  local lineno=0 raw kind rest ere remedy pattern dup
  local wsrc were wcount wreason wrest wrest2 wrest3
  local term_keys=""

  while IFS= read -r raw || [[ -n "$raw" ]]; do
    lineno=$((lineno + 1))
    raw="${raw%$'\r'}"
    case "$raw" in
      '' | '#'*) continue ;;
    esac
    kind="${raw%%$'\t'*}"

    case "$kind" in
      TERM)
        rest="${raw#*$'\t'}"
        ere="${rest%%$'\t'*}"
        remedy="${rest#*$'\t'}"
        if [[ "$rest" == "$raw" || "$remedy" == "$rest" || -z "$ere" || -z "$remedy" ]]; then
          report BAD-TERM "$lineno" \
            "${STALE_TERMS}: TERM needs exactly 3 tab-separated fields: TERM<TAB>ere<TAB>remedy"
          continue
        fi
        if contains_line "$term_keys" "$ere"; then
          report BAD-TERM "$lineno" "${STALE_TERMS}: term '${ere}' is already declared"
          continue
        fi

        pattern="$ere"
        if [[ "$ere" == "@FORMER_REPO_CLONE_URLS@" ]]; then
          if ! pattern="$(derive_former_repo_urls)"; then
            report BAD-TERM "$lineno" \
              "${STALE_TERMS}: '@FORMER_REPO_CLONE_URLS@' derived ZERO repo names from ${ROOT}/docs/reference/former-repos.md. A term that matches nothing is indistinguishable from a clean tree — fix that page's \`bobmatnyc/<repo>\` list, or delete this term."
            continue
          fi
        fi

        if ! ere_compiles "$pattern"; then
          report BAD-TERM "$lineno" \
            "${STALE_TERMS}: '${ere}' is not a valid POSIX extended regular expression"
          continue
        fi

        term_keys="${term_keys}${ere}"$'\n'
        STALE_TERM_LIST="${STALE_TERM_LIST}${ere}"$'\t'"${pattern}"$'\t'"${remedy}"$'\n'
        STALE_TERM_COUNT=$((STALE_TERM_COUNT + 1))
        ;;

      WAIVE)
        wrest="${raw#*$'\t'}"
        wsrc="${wrest%%$'\t'*}"
        wrest2="${wrest#*$'\t'}"
        were="${wrest2%%$'\t'*}"
        wrest3="${wrest2#*$'\t'}"
        wcount="${wrest3%%$'\t'*}"
        wreason="${wrest3#*$'\t'}"
        if [[ "$wrest" == "$raw" || "$wrest2" == "$wrest" || "$wrest3" == "$wrest2" ||
          "$wreason" == "$wrest3" || -z "$wsrc" || -z "$were" || -z "$wcount" ]]; then
          report BAD-WAIVER "$lineno" \
            "${STALE_TERMS}: WAIVE needs exactly 5 tab-separated fields: WAIVE<TAB>source<TAB>ere<TAB>count<TAB>reason"
          continue
        fi
        # An unexplained waiver is indistinguishable from an oversight a year
        # later, which is exactly when someone has to decide whether to keep it.
        if [[ -z "${wreason//[[:space:]]/}" ]]; then
          report BAD-WAIVER "$lineno" \
            "${STALE_TERMS}: waiver for '${wsrc}' / '${were}' has an empty reason. Say why the occurrence is legitimate, or delete the row."
          continue
        fi
        case "$wcount" in
          '' | *[!0-9]*)
            report BAD-WAIVER "$lineno" \
              "${STALE_TERMS}: waiver for '${wsrc}' / '${were}' has count '${wcount}', which is not a non-negative integer"
            continue
            ;;
        esac
        if [[ "$wcount" -lt 1 ]]; then
          report BAD-WAIVER "$lineno" \
            "${STALE_TERMS}: waiver for '${wsrc}' / '${were}' waives 0 occurrences, which waives nothing. Delete the row."
          continue
        fi
        dup="$(waiver_line "$wsrc" "$were")"
        if [[ "$dup" != "0" ]]; then
          report BAD-WAIVER "$lineno" \
            "${STALE_TERMS}: '${wsrc}' / '${were}' is already waived on line ${dup}. One waiver per source/term pair — the second would be silently ignored."
          continue
        fi
        STALE_WAIVERS="${STALE_WAIVERS}${lineno}"$'\t'"${wsrc}"$'\t'"${were}"$'\t'"${wcount}"$'\t'"${wreason}"$'\n'
        ;;

      *)
        report BAD-RECORD "$lineno" \
          "${STALE_TERMS}: unknown record type '${kind}' (expected TERM or WAIVE)"
        ;;
    esac
  done <"$STALE_TERMS"
}

# Fails every waiver that can no longer refer to anything: a source no PAGE row
# publishes, or an ERE no TERM row declares. Both are dead weight that reads as
# active protection.
stale_check_dead_waivers() {
  local term_keys wl wsrc were wcount wreason
  term_keys="$(printf '%s' "$STALE_TERM_LIST" | cut -f1)"
  while IFS=$'\t' read -r wl wsrc were wcount wreason; do
    [[ -n "$wl" ]] || continue
    if ! contains_line "$sources" "$wsrc"; then
      report BAD-WAIVER "$wl" \
        "${STALE_TERMS}: waiver names '${wsrc}', which no PAGE row publishes. Dead waiver — delete the row."
      continue
    fi
    if ! contains_line "$term_keys" "$were"; then
      report BAD-WAIVER "$wl" \
        "${STALE_TERMS}: waiver names ERE '${were}', which no TERM row declares. Dead waiver — delete the row."
    fi
  done <<<"$STALE_WAIVERS"
}

# Searches every published page for every term, enforcing the waiver ratchet.
stale_scan_pages() {
  local mline msrc tkey tpat tremedy hits mcount waived note pline h wl

  while IFS=$'\t' read -r mline msrc; do
    [[ -n "$mline" ]] || continue
    while IFS=$'\t' read -r tkey tpat tremedy; do
      [[ -n "$tkey" ]] || continue

      hits="$(grep -nE -- "$tpat" "${ROOT}/${msrc}" || true)"
      if [[ -z "$hits" ]]; then
        mcount=0
      else
        mcount="$(printf '%s\n' "$hits" | grep -c '' | tr -d ' ')"
      fi
      waived="$(waiver_count "$msrc" "$tkey")"

      [[ "$mcount" -eq "$waived" ]] && continue

      # Ratchet, both directions — same mechanic as .line-cap-allowlist.tsv.
      # N-1 is a finding, not a quiet win: a waiver that over-states is a
      # standing licence to reintroduce what it no longer covers.
      if [[ "$mcount" -lt "$waived" ]]; then
        wl="$(waiver_line "$msrc" "$tkey")"
        report BAD-WAIVER "$wl" \
          "${STALE_TERMS}: waiver allows ${waived} occurrence(s) of '${tkey}' in '${msrc}', but only ${mcount} remain. Lower the count to ${mcount}, or delete the row if it is 0."
        continue
      fi

      note=""
      [[ "$waived" -gt 0 ]] && note=" [${waived} occurrence(s) waived, ${mcount} now present — raising the waiver is not the fix]"
      while IFS= read -r h; do
        [[ -n "$h" ]] || continue
        pline="${h%%:*}"
        report STALE "$mline" \
          "page '${msrc}' line ${pline} contains retired term '${tkey}' — ${tremedy}${note}"
      done <<<"$hits"
    done <<<"$STALE_TERM_LIST"
  done <<<"$published"
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
        *"$FORBIDDEN_SUFFIX")
          report FORBIDDEN "$lineno" \
            "source '${src}' is named '*${FORBIDDEN_SUFFIX}', the suffix that marks a docs/ file as never-public. Either rename the file without the suffix if it is meant to be published, or delete this row."
          continue
          ;;
      esac

      # ... then the top-level trees by prefix ...
      while IFS= read -r tree; do
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
        report FORBIDDEN "$lineno" \
          "source '${src}' is inside the DO-NOT-PUBLISH tree '${forbidden_hit}'. That tree is never published; delete this row."
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
        published="${published}${lineno}"$'\t'"${src}"$'\n'
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

# The STALE pass runs LAST and only over pages that cleared every check above,
# so it never reports on a source the manifest could not resolve in the first
# place. It is opt-in (issue #5125) — see the header for why it is wired to
# nothing yet.
stale_scanned=0
manifest_fail="$fail" # findings raised before the STALE pass began
if [[ "$STALE_MODE" -eq 1 ]]; then
  [[ -n "$STALE_TERMS" ]] || STALE_TERMS="${ROOT}/docs/public-stale-terms.tsv"
  if [[ ! -f "$STALE_TERMS" ]]; then
    echo "FAIL: stale-terms file not found: ${STALE_TERMS}" >&2
    exit 2
  fi

  stale_load_terms

  # Same premise as the page floor above: a terms file that declares nothing
  # searches for nothing, and "found no stale content" would be a lie.
  if [[ "$STALE_TERM_COUNT" -lt 1 ]]; then
    echo "FAIL: SCAN FLOOR — ${STALE_TERMS} declares 0 term(s)." >&2
    echo "      Nothing was searched for, so the STALE check could not have failed." >&2
    exit 1
  fi

  stale_check_dead_waivers
  stale_scan_pages
  stale_scanned=1
fi

if [[ "$fail" -ne 0 ]]; then
  if [[ "$stale_scanned" -eq 1 ]]; then
    cat >&2 <<EOF

Retired names and anti-patterns are declared in ${STALE_TERMS}.
Fix the PAGE — a published page is read by people who cannot check it against
the source tree. Waive an occurrence only when it is genuinely correct (naming a
historical artifact by its recorded name, for instance), with a reason, and
never by raising an existing waiver's count.
EOF
  fi
  if [[ "$manifest_fail" -ne 0 ]]; then
    cat >&2 <<EOF

The public documentation manifest is an ALLOWLIST (${MANIFEST}).
Every PAGE row must name an existing .md file under docs/ that is outside the
DO-NOT-PUBLISH trees and not named \`*-internal.md\`, with a unique route. Fix the
rows above — never widen the boundary to make this gate green.

Both rules, and the \`*-internal.md\` naming convention for internal docs that
live in a mixed-audience directory, are documented in that file's header.
EOF
  fi
  exit 1
fi

if [[ "$stale_scanned" -eq 1 ]]; then
  echo "public-docs gate: ${page_count} page(s) across ${section_count} section(s) — all resolve inside the public boundary, and none carries any of the ${STALE_TERM_COUNT} retired term(s) in ${STALE_TERMS}."
else
  echo "public-docs gate: ${page_count} page(s) across ${section_count} section(s) — all resolve inside the public boundary."
fi
