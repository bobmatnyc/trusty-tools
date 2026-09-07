#!/usr/bin/env bash
#
# check_doc_paths.sh — backtick path-citation gate (issue #5147).
#
# Why: a doc that cites `crates/trusty-search/src/mcp/tools.rs` is making a
#   checkable claim, and nothing checked it. The doc/code consistency sweep
#   (#5136 / PR #5137) found 18 broken backtick path citations outside the
#   archival trees — five of them in crates/trusty-search/CLAUDE.md, a file
#   agents read every session, all of them left behind when a 500-SLOC split
#   turned a module file into a directory. A reader who opens the cited path
#   gets nothing, and an agent that greps for it concludes the code is missing.
#   Resolving a backtick-quoted path against the filesystem needs no human
#   judgment, so it belongs in a gate for the same reason the SLOC cap does
#   (scripts/check_line_cap.sh, #610): advice without a gate loses.
#
#   This is the path half of what scripts/check_public_docs.sh --stale does for
#   retired names (#5125 / PR #5128). That check proves a published page IS
#   THERE; neither proves the other, and both are needed.
#
# What: scans a fixed set of LIVE, actively-maintained Markdown files for
#   backtick-quoted tokens beginning `crates/`, `src/`, `scripts/`, `docs/` or
#   `.github/`, and fails on any that does not resolve in the checkout.
#
#   Findings, one line each on stderr:
#     FAIL BROKEN <file>:<line>: <token> — …   the path does not exist
#     WARN LINE   <file>:<line>: <token> — …   the file exists but the `:N`
#                                              suffix is past its last line
#
#   The WARN is deliberately not a failure. A stale line number still lands the
#   reader in the right file, which is most of the citation's value, and line
#   numbers drift on every edit to the cited file — failing on drift would make
#   the gate fire constantly for the smallest possible payoff. A missing FILE is
#   categorically different: there is nothing to land in.
#
# SCOPE — what is scanned, and why the rest is not:
#     CLAUDE.md, README.md                 repo root
#     docs/reference/**/*.md               the current-state reference set
#     docs/architecture/**/*.md            the current-state architecture set
#     crates/*/CLAUDE.md                   per-crate agent instructions
#     crates/*/README.md                   per-crate entry points
#
#   Everything else under docs/ is excluded ON PURPOSE, not for convenience. An
#   ADR, a spec, a research note, a release readiness doc and a session log all
#   RECORD A PAST STATE: docs/adr/0013 must keep saying `crates/trusty-controller/`
#   because that is what the crate was called when the decision was taken.
#   "Fixing" those citations would falsify the record. Same reasoning as
#   check_public_docs.sh's excluded trees — historical names belong in
#   historical documents.
#
#   The crate globs are depth-1 by design. crates/*/*/README.md reaches into
#   bundled asset trees (crates/trusty-agents/.trusty-agents/skills/**) whose
#   READMEs describe a payload shipped elsewhere, so their paths are not this
#   checkout's paths.
#
# EXCLUDED TOKENS — a path-SHAPED token is not always a path citation. Each rule
#   below is a class the sweep flagged, or would have flagged, as a false
#   positive (issue #5147 names the constructed-name class explicitly). A token
#   is skipped when it contains any of:
#     <  >        a placeholder            `crates/<crate>/changelog.d/`
#     $           shell/env interpolation  `docs/$CRATE/README.md`
#     {  }        brace expansion, or a Rust/format template
#                                          `crates/{a,b}/Cargo.toml`
#     *  ?        a glob                   `crates/*/Cargo.toml`
#     ...  …      an elision               `crates/trusty-search/src/…`
#     ::          a Rust module path, not a filesystem path
#                                          `crates/tc-services::cto_db`
#     |           an alternation, or a markdown table cell escape
#   …and one structural rule:
#     a bare `src/…` token in a file that is not inside a crate. `src/` is
#     crate-relative by convention, so docs/architecture/harnesses.md naming
#     `src/tools/` in a per-crate table has no single crate to resolve against.
#     Inside a crate there is exactly one, and the token IS checkable — which is
#     the case the issue is actually about.
#
#   A `:LINE` or `:LINE-LINE` suffix and a `#anchor` suffix are stripped before
#   the existence check; `:LINE` is then re-checked against the file's length.
#
# Usage:
#   bash scripts/check_doc_paths.sh              # scan the checkout
#   bash scripts/check_doc_paths.sh --root <dir> # scan a fixture tree (self-test)
#   bash scripts/check_doc_paths.sh --help
#
# Exit: 0 when every citation resolves (warnings do not fail); 1 on any BROKEN
#   finding or on an empty scan; 2 on a usage error.
#
# Test: scripts/check_doc_paths_selftest.sh — fixture trees under
#   scripts/test-data/doc-paths/ cover a broken citation, a clean file, every
#   excluded-token class, the crate-relative `src/` resolution, and the
#   line-overrun warning. It also asserts the committed tree passes.
#
# Portability: bash 3.2 (macOS system bash) and bash 5 (Linux CI). POSIX tools
#   only — no associative arrays, no jq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$REPO_ROOT"
FIXTURE_MODE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)
      [[ $# -lt 2 ]] && {
        echo "ERROR: --root needs a path" >&2
        exit 2
      }
      ROOT="$(cd "$2" 2>/dev/null && pwd)" || {
        echo "ERROR: --root '$2' is not a directory" >&2
        exit 2
      }
      FIXTURE_MODE=1
      shift 2
      ;;
    -h | --help)
      sed -n '2,92p' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

[[ "$ROOT" == "$REPO_ROOT" ]] && FIXTURE_MODE=0

fail=0
broken=0
warned=0
scanned=0

# Why: the real run must scan what is COMMITTED, so an untracked scratch file
#   cannot fail someone else's commit. A fixture tree cannot use that rule — a
#   fixture is untracked while it is being written, `git ls-files` would return
#   nothing, and the scan floor would then fire on the self-test instead of on
#   the thing it is meant to catch.
# What: prints the in-scope Markdown paths, relative to $ROOT, one per line.
# Test: scripts/check_doc_paths_selftest.sh — every case runs in fixture mode,
#   and the final case runs the tracked-file path over the real checkout.
list_scanned_files() {
  if [[ "$FIXTURE_MODE" -eq 1 ]]; then
    (cd "$ROOT" && find . -name '*.md' -type f | sed 's|^\./||' | sort) | in_scope_filter
  else
    (cd "$ROOT" && git ls-files '*.md') | in_scope_filter
  fi
}

# Why: `case`'s `*` matches `/` as well, so a `crates/*/README.md` glob would
#   also admit crates/trusty-agents/.trusty-agents/skills/cto-db/python/README.md
#   — a bundled asset payload, five levels down, whose paths are not this
#   checkout's paths. The depth-1 rule needs a pattern that cannot cross `/`.
# What: prints only the in-scope paths from stdin. See SCOPE in the header.
in_scope_filter() {
  local f
  while IFS= read -r f; do
    if [[ "$f" =~ ^(CLAUDE|README)\.md$ ]] ||
      [[ "$f" =~ ^docs/reference/.*\.md$ ]] ||
      [[ "$f" =~ ^docs/architecture/.*\.md$ ]] ||
      [[ "$f" =~ ^crates/[^/]+/(CLAUDE|README)\.md$ ]]; then
      printf '%s\n' "$f"
    fi
  done
}

# Prints the crate root (relative to $ROOT) containing $1, or nothing.
crate_root_of() {
  local d
  d="$(dirname "$1")"
  while [[ "$d" != "." && "$d" != "/" ]]; do
    if [[ -f "${ROOT}/${d}/Cargo.toml" ]]; then
      printf '%s' "$d"
      return 0
    fi
    d="$(dirname "$d")"
  done
  return 1
}

report_broken() {
  echo "FAIL BROKEN $1:$2: $3 — no such file or directory in this checkout" >&2
  broken=$((broken + 1))
  fail=1
}

report_warn() {
  echo "WARN LINE $1:$2: $3 — $4 has only $5 line(s)" >&2
  warned=$((warned + 1))
}

# Extracts backtick-quoted, path-prefixed tokens from one file as
# "<line><TAB><token>". Fenced code blocks are skipped: inside a fence there are
# no inline code spans to quote a citation, and a fence's own delimiter line
# would otherwise be parsed as one.
extract_tokens() {
  awk '
    /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
    fence { next }
    {
      line = $0
      while (match(line, /`[^`]+`/)) {
        span = substr(line, RSTART + 1, RLENGTH - 2)
        line = substr(line, RSTART + RLENGTH)
        n = split(span, words, /[[:space:]]+/)
        for (i = 1; i <= n; i++) {
          w = words[i]
          # Trailing prose punctuation a citation picks up inside a span. A
          # trailing `.` is stripped one at a time and never off a token already
          # ending `..`, so an ASCII elision (`crates/…/core/...`) survives to
          # reach the exclusion rules instead of being silently repaired into a
          # directory path that does not exist.
          do {
            prev = w
            sub(/[,;)\]]$/, "", w)
            if (w !~ /\.\.$/) sub(/\.$/, "", w)
          } while (w != prev)
          if (w ~ /^(crates|src|scripts|docs|\.github)\//)
            printf "%d\t%s\n", NR, w
        }
      }
    }
  ' "$1"
}

while IFS= read -r doc; do
  [[ -n "$doc" ]] || continue
  scanned=$((scanned + 1))

  while IFS=$'\t' read -r lineno token; do
    [[ -n "$token" ]] || continue

    # See EXCLUDED TOKENS in the header. Each pattern is one documented class.
    case "$token" in
      *'<'* | *'>'* | *'$'* | *'{'* | *'}'* | *'*'* | *'?'* | \
        *'...'* | *'…'* | *'::'* | *'|'*) continue ;;
    esac

    path="$token"
    path="${path%%#*}" # `docs/specs/foo.md#SPEC-X` → docs/specs/foo.md
    want_line=""
    case "$path" in
      *:[0-9]*)
        want_line="${path#*:}"
        want_line="${want_line%%-*}" # `…:81-104` → 81
        path="${path%%:*}"
        ;;
    esac
    [[ -n "$path" ]] || continue

    base=""
    case "$path" in
      src/*)
        # Crate-relative by convention — see EXCLUDED TOKENS. No crate, no claim.
        base="$(crate_root_of "$doc")" || continue
        base="${base}/"
        ;;
    esac

    if [[ ! -e "${ROOT}/${base}${path}" ]]; then
      report_broken "$doc" "$lineno" "$token"
      continue
    fi

    if [[ -n "$want_line" && -f "${ROOT}/${base}${path}" ]]; then
      total="$(grep -c '' "${ROOT}/${base}${path}" | tr -d ' ')"
      if [[ "$want_line" -gt "$total" ]]; then
        report_warn "$doc" "$lineno" "$token" "${base}${path}" "$total"
      fi
    fi
  done < <(extract_tokens "${ROOT}/${doc}")
done < <(list_scanned_files)

# Scan floor, same premise as check_public_docs.sh's and
# check_changelog_fragment.sh's (#4618): "nothing was scanned" and "everything
# scanned was clean" produce the same exit status, and only one of them is a
# passing gate. An empty scan means a bad --root or a broken file list.
if [[ "$scanned" -lt 1 ]]; then
  echo "FAIL: SCAN FLOOR — no in-scope Markdown file was scanned under ${ROOT}." >&2
  echo "      Nothing was examined, so this gate could not have failed." >&2
  exit 1
fi

if [[ "$fail" -ne 0 ]]; then
  cat >&2 <<'EOF'

A backtick-quoted path citation names a file that is not in this checkout.
Fix the CITATION — a reader who opens it gets nothing, and an agent that greps
for it concludes the code was deleted. The usual cause is a 500-SLOC split that
turned `foo.rs` into `foo/` and left the docs behind.

Never widen this gate to make it green. If a token is genuinely not a literal
path (a placeholder, a glob, a Rust module path), write it in one of the shapes
the header's EXCLUDED TOKENS list already covers.
EOF
  exit 1
fi

summary="doc-paths gate: ${scanned} file(s) scanned — every backtick path citation resolves"
if [[ "$warned" -gt 0 ]]; then
  summary="${summary} (${warned} stale line-number warning(s) above)"
fi
echo "${summary}."
