#!/usr/bin/env bash
#
# check_test_pointers.sh — lint `Test:` doc-comment pointers against real
# test names (issue #2458).
#
# Why: the Why/What/Test doc pattern (CLAUDE.md) requires every public item
#   to carry a `/// Test:` (or `//! Test:` at module level) pointer naming the
#   test(s) that exercise it. Those pointers rot silently — a test gets
#   renamed or deleted and the doc comment is never updated, because nothing
#   mechanically checks the cross-reference. Three dangling pointers were
#   caught by human reviewers in a single 48h window (PR #2499: cli.rs cited
#   a `cli_url_unset_is_none` test that was never named that; PR #2513: two
#   pointers in poll/mod.rs cited a test name variant that didn't match the
#   real one). This script turns that into a mechanical CI gate, mirroring
#   scripts/check_line_cap.sh's ratcheted-allowlist shape.
#
# What: scans every tracked `.rs` file (`git ls-files '*.rs'`, minus
#   EXCLUDE_PREFIXES below) for `Test:` doc-comment annotations (a `///`/`//!`
#   line whose stripped content starts with the literal `Test:`, plus any
#   immediately-following `///`/`//!` continuation lines up to the next blank
#   comment line, a new `Why:`/`What:` tag, or the end of the doc block).
#   Within that text, ONLY the LEADING run of backtick-quoted spans
#   (`` `name` ``, separated by whitespace/`,`/`;`/`&`/"and"/"plus") is
#   scanned — extraction stops dead at the first token that isn't a
#   backtick-span or separator (see candidate_names' doc comment for why:
#   real annotations routinely continue past the test-name list into prose
#   that ALSO backtick-quotes unrelated symbols). Each surviving span is
#   then filtered:
#     - module-path-qualified names (`foo::bar::baz_test`) are matched on
#       their FINAL segment (`baz_test`);
#     - a citation is only linted when its final segment is a lowercase
#       snake_case identifier (`^[a-z][a-z0-9_]*$`) AND contains at least one
#       `_` — this excludes glob patterns (`cli_parses_issue_*`, rejected by
#       the character class) and bare module/type references (`` `tests` ``,
#       `` `ActivityInfo` `` — rejected for having no underscore or for
#       CamelCase).
#   Each surviving candidate is checked with `git grep` (cached per crate —
#   see the fn-name cache section) for a `fn <name>(` (or `fn <name><...>(`
#   for a generic fn) definition anywhere under the citing file's own crate
#   (the nearest ancestor directory containing a Cargo.toml; falls back to
#   the whole tree if none is found). No match = a dangling pointer,
#   reported as `file:line: cites <name> (crate <dir>)`.
#
# PRECISION LIMITS (documented per issue #2458, "pragmatic grep is
#   acceptable"):
#   - citations must be backtick-quoted (this codebase's universal doc
#     convention for code identifiers); a bare-word citation with no
#     backticks is never linted.
#   - only the LEADING backtick-run after "Test:" is linted; a second real
#     test name mentioned after ordinary prose (rather than a `,`/`and`/
#     `plus`-joined list) is a false NEGATIVE — missed, not flagged. This
#     trades missed rot for not blocking merges on misparsed prose; the same
#     leans-lenient tradeoff check_line_cap.sh's SLOC counter documents.
#   - the existence check is `fn <name>(` anywhere in the crate — it does
#     NOT verify the function is under `#[test]` / `#[cfg(test)]`. A
#     dangling pointer that happens to collide with a same-named production
#     function will be missed (false negative).
#   - "same crate" is filesystem-nearest-Cargo.toml, not `cargo metadata`
#     dependency resolution — a citation naming a test in a *different*
#     crate than the one containing the doc comment will be flagged even if
#     the test genuinely exists elsewhere. This is intentional: Test:
#     pointers are meant to name in-crate tests; cross-crate citations
#     should name the crate in prose (outside the leading backtick run, so
#     it is never scanned) rather than as a bare backtick span.
#
# ALLOWLIST (ratchet, can only shrink — mirrors .line-cap-allowlist.tsv):
#   `.test-pointer-allowlist.tsv`, one `<path><TAB><line><TAB><name>` row per
#   grandfathered dangling pointer. A listed pointer that is STILL dangling
#   is suppressed (OK). A listed pointer that is NO LONGER dangling (fixed,
#   or the doc comment was corrected/removed) is a FAIL — remove its entry
#   (`--update` does this automatically). Ordinary `--update` never ADDS a
#   row; `--seed` is the one-time bootstrap that grandfathers every
#   currently-dangling pointer in one shot (refuses to run if the allowlist
#   already has rows, to avoid clobbering hand-curated entries) — used once,
#   in the PR that introduced this gate, to grandfather the large body of
#   pre-existing drift a first-ever run surfaces across a codebase this
#   size. Prefer fixing the doc comment over adding a new row by hand.
#
# Usage:
#   check_test_pointers.sh              # check mode (default); exit 0 = clean
#   check_test_pointers.sh --update      # prune allowlist entries that are no
#                                         # longer dangling (ratchet down only)
#   check_test_pointers.sh --seed        # bootstrap: grandfather every
#                                         # currently-dangling pointer (refuses
#                                         # if the allowlist already has rows)
#   check_test_pointers.sh --self-test   # run the fixture-based self-test
#                                         # suite (see run_self_test below)
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); POSIX tools + git only.

set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"

# ---------------------------------------------------------------------------
# Doc-comment "Test:" block extraction, shared by both scan() and the
# self-test. Reads a single file on stdin-free `awk -f`-style invocation via
# a shell function so it can be reused without spawning a child script.
#
# Output: one row per Test: annotation found: "<line>\t<blob>" where <blob>
# is the concatenated text of the annotation (first line + any continuation
# lines), backtick spans intact.
# ---------------------------------------------------------------------------
EXTRACT_AWK='
function is_doc(line) { return (line ~ /^[ \t]*(\/\/\/|\/\/!)/) }
function strip(line,    s) {
  s = line
  sub(/^[ \t]*(\/\/\/|\/\/!)[ \t]?/, "", s)
  return s
}
BEGIN { collecting = 0; buf = ""; bufstart = "" }
{
  line = $0
  handled = 0
  if (collecting) {
    if (is_doc(line)) {
      content = strip(line)
      if (content ~ /^Test:/) {
        print bufstart "\t" buf
        buf = content; bufstart = NR
        handled = 1
      } else if (content ~ /^(Why|What):/ || content == "") {
        print bufstart "\t" buf
        collecting = 0; buf = ""; bufstart = ""
        handled = 1
      } else {
        buf = buf " " content
        handled = 1
      }
    } else {
      print bufstart "\t" buf
      collecting = 0; buf = ""; bufstart = ""
    }
  }
  if (!handled && !collecting) {
    if (is_doc(line)) {
      content = strip(line)
      if (content ~ /^Test:/) {
        collecting = 1
        buf = content
        bufstart = NR
      }
    }
  }
}
END { if (collecting) print bufstart "\t" buf }
'

extract_test_blocks() {
  awk "$EXTRACT_AWK" "$1"
}

# ---------------------------------------------------------------------------
# candidate_names: given a Test: blob, print one lowercase-final-segment
# candidate per line (may print duplicates; caller de-dupes if it cares).
#
# Critically, this only walks the LEADING run of backtick-quoted spans
# immediately after "Test:" (separated by whitespace/`,`/`;`/`&`/"and"/
# "plus"), and stops at the first token that isn't one of those. Real-world
# annotations routinely continue past the test-name list into prose that
# ALSO backtick-quotes unrelated symbols — field names, module names, or the
# item's own name — e.g. "Test: Used as the `pkg_mgr` field of `PlatformInfo`
# in `tests::hint_*`." (no real test cited at all) or "Test:
# `commit_shas_gated_on_merged_at` exercises `merged_at` / `merge_commit_sha`."
# (only the first span is a test name; the rest are fields being exercised).
# Scanning every backtick in the whole blob made those false positives; the
# leading-run restriction accepts a false NEGATIVE instead (a second real
# test name buried after prose is missed) which is the safe direction for a
# blocking gate, matching check_line_cap.sh's stated leniency philosophy.
# ---------------------------------------------------------------------------
CANDIDATES_AWK='
function emit(nm,    parts, k, final) {
  k = split(nm, parts, "::")
  final = parts[k]
  if (final == "") return
  if (final !~ /^[a-z][a-z0-9_]*$/) return
  if (final !~ /_/) return
  print final
}
{
  s = $0
  sub(/^Test:[ \t]*/, "", s)
  n = length(s)
  i = 1
  while (i <= n) {
    c = substr(s, i, 1)
    if (c == "`") {
      j = index(substr(s, i + 1), "`")
      if (j == 0) break
      emit(substr(s, i + 1, j - 1))
      i = i + 1 + j
    } else if (c == " " || c == "\t" || c == "," || c == ";" || c == "&") {
      i++
    } else {
      rest = substr(s, i)
      if (rest ~ /^and([ \t]|$)/)       { i += 3 }
      else if (rest ~ /^plus([ \t]|$)/) { i += 4 }
      else { break }
    }
  }
}
'

candidate_names() {
  printf '%s\n' "$1" | awk "$CANDIDATES_AWK"
}

# ---------------------------------------------------------------------------
# crate_root_for: print the nearest ancestor directory (relative to the repo
# root, cwd) containing a Cargo.toml for a given repo-relative file path.
# Falls back to "." (workspace root) if none is found above it.
# ---------------------------------------------------------------------------
crate_root_for() {
  local f="$1" dir
  dir="$(dirname "$f")"
  while [ "$dir" != "." ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/Cargo.toml" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  printf '%s\n' "."
}

# ---------------------------------------------------------------------------
# Per-crate `fn` name cache. Spawning a `git grep` per CITATION does not
# scale (thousands of citations workspace-wide); instead, the first time a
# given crate root is queried in a scan() run, we grep it ONCE for every
# `fn <ident>` occurrence and cache the resulting name set to a temp file.
# Every subsequent lookup for that crate is then a local `grep -x` against
# the cached set — O(crates) subprocess spawns instead of O(citations).
# ---------------------------------------------------------------------------
CRATE_FN_CACHE_DIR=""

crate_cache_file() {
  local crate="$1" key
  key="$(printf '%s' "$crate" | tr -c 'A-Za-z0-9' '_')"
  printf '%s/%s.fns\n' "$CRATE_FN_CACHE_DIR" "$key"
}

# ---------------------------------------------------------------------------
# name_exists_in_crate: 0 (true) if `fn <name>` (as a whole identifier,
# followed by `(` or `<` for a generic fn) is defined anywhere among tracked
# .rs files under crate root $1. Builds/reuses the crate's fn-name cache.
# ---------------------------------------------------------------------------
name_exists_in_crate() {
  local crate="$1" name="$2" cache
  cache="$(crate_cache_file "$crate")"
  if [ ! -f "$cache" ]; then
    git grep -ohE 'fn[[:space:]]+[a-zA-Z_][a-zA-Z0-9_]*[[:space:]]*[(<]' -- "${crate}/*.rs" 2>/dev/null \
      | sed -E -e 's/^fn[[:space:]]+//' -e 's/[[:space:]]*[(<]$//' \
      | sort -u > "$cache" || : > "$cache"
  fi
  grep -qxF "$name" "$cache"
}

# ---------------------------------------------------------------------------
# EXCLUDE_PREFIXES: repo-relative path prefixes skipped entirely by the
# scan. These are directories whose own Cargo.toml declares a standalone
# (non-member) `[workspace]` table — i.e. they explicitly opt OUT of the
# trusty-tools workspace and its conventions:
#   - crates/trusty-search/tests/benchmark_corpus/synthetic: a synthetic,
#     LLM-authored fixture crate (see its own README: "Not part of the
#     workspace") used purely as non-circular search-benchmark corpus text.
#     It imitates the Why/What/Test doc style closely enough to read as
#     realistic chunking input, so it "cites" fake test names by design —
#     linting it would be grading fiction, not real doc drift.
# ---------------------------------------------------------------------------
is_excluded_path() {
  case "$1" in
    crates/trusty-search/tests/benchmark_corpus/synthetic/*) return 0 ;;
    *) return 1 ;;
  esac
}

# ---------------------------------------------------------------------------
# scan: run the full lint over the git repo rooted at cwd. Writes violation
# lines ("<path>\t<line>\t<name>\t<crate>") to the file named by $1, and
# stale-allowlist-entry lines ("<path>\t<line>\t<name>") to $2. Both files
# are truncated first. Returns nothing (caller inspects the files).
# ---------------------------------------------------------------------------
scan() {
  local viol_out="$1" stale_out="$2"
  : > "$viol_out"
  : > "$stale_out"

  CRATE_FN_CACHE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tpfncache.XXXXXX")"

  local allowlist=".test-pointer-allowlist.tsv"

  git ls-files '*.rs' | while IFS= read -r f; do
    [ -n "$f" ] || continue
    [ -f "$f" ] || continue
    is_excluded_path "$f" && continue
    local crate
    crate="$(crate_root_for "$f")"
    extract_test_blocks "$f" | while IFS=$'\t' read -r line blob; do
      [ -n "${line:-}" ] || continue
      local seen_file
      seen_file="$(mktemp "${TMPDIR:-/tmp}/tpseen.XXXXXX")"
      candidate_names "$blob" | while IFS= read -r name; do
        [ -n "$name" ] || continue
        # De-dupe within a single annotation (same name cited twice in one blob).
        if grep -qxF "$name" "$seen_file" 2>/dev/null; then continue; fi
        printf '%s\n' "$name" >> "$seen_file"
        if ! name_exists_in_crate "$crate" "$name"; then
          printf '%s\t%s\t%s\t%s\n' "$f" "$line" "$name" "$crate" >> "$viol_out"
        fi
      done
      rm -f "$seen_file"
    done
  done

  # Apply the allowlist: any violation matching an allowlist row is
  # suppressed; any allowlist row that is NOT among current violations is
  # stale (the pointer was fixed, or no longer exists) and must be pruned.
  if [ -f "$allowlist" ]; then
    local filtered
    filtered="$(mktemp "${TMPDIR:-/tmp}/tpfilt.XXXXXX")"
    : > "$filtered"
    while IFS=$'\t' read -r p l n c; do
      [ -n "${p:-}" ] || continue
      if grep -F -q -x -- "$(printf '%s\t%s\t%s' "$p" "$l" "$n")" \
           <(awk -F'\t' '$0 !~ /^#/ && NF>=3 {print $1"\t"$2"\t"$3}' "$allowlist") 2>/dev/null; then
        : # allowlisted — suppress
      else
        printf '%s\t%s\t%s\t%s\n' "$p" "$l" "$n" "$c" >> "$filtered"
      fi
    done < "$viol_out"
    mv "$filtered" "$viol_out"

    # Stale detection: for every allowlist row, check whether it is STILL a
    # genuine dangling pointer by re-deriving candidates fresh — an
    # allowlist row is stale if the cited name now resolves (fixed) or the
    # doc line no longer cites it at all.
    while IFS=$'\t' read -r p l n; do
      [ -n "${p:-}" ] || continue
      [ -f "$p" ] || { printf '%s\t%s\t%s\n' "$p" "$l" "$n" >> "$stale_out"; continue; }
      local crate2 blob2 still_bad
      crate2="$(crate_root_for "$p")"
      blob2="$(extract_test_blocks "$p" | awk -F'\t' -v want="$l" '$1==want {sub(/^[^\t]*\t/,""); print}')"
      still_bad=0
      if [ -n "$blob2" ]; then
        while IFS= read -r cand; do
          [ "$cand" = "$n" ] || continue
          if ! name_exists_in_crate "$crate2" "$n"; then still_bad=1; fi
        done < <(candidate_names "$blob2")
      fi
      if [ "$still_bad" -eq 0 ]; then
        printf '%s\t%s\t%s\n' "$p" "$l" "$n" >> "$stale_out"
      fi
    done < <(awk -F'\t' '$0 !~ /^#/ && NF>=3 {print $1"\t"$2"\t"$3}' "$allowlist")
  fi

  rm -rf "$CRATE_FN_CACHE_DIR"
  CRATE_FN_CACHE_DIR=""
}

# ---------------------------------------------------------------------------
# run_self_test: builds a throwaway git repo with a fixture crate and
# asserts the checker correctly passes a valid pointer and fails a dangling
# one. Mirrors the level of self-verification check_line_cap.sh has (none,
# beyond being exercised by its introducing PR) by giving this script at
# least one mechanical regression guard, since the extraction logic here is
# materially more complex (multi-line blocks, backtick parsing).
# ---------------------------------------------------------------------------
run_self_test() {
  local tmp ok=1
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/tp-selftest.XXXXXX")"
  trap 'rm -rf "$tmp"' RETURN

  mkdir -p "$tmp/crates/fixture/src"
  cat > "$tmp/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/fixture"]
resolver = "2"
EOF
  cat > "$tmp/crates/fixture/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
EOF
  cat > "$tmp/crates/fixture/src/lib.rs" <<'EOF'
/// Why: fixture for check_test_pointers.sh self-test.
/// What: does nothing.
/// Test: `real_test_exists`, `dangling_test_missing`.
pub fn widget() {}

/// Test: covered by prose with no identifiers, and a glob `widget_*` and a
/// module ref `some::module`.
pub fn untested_by_design() {}

/// Test: Used as the `field_name` field of `Widget` in `tests::hint_*`.
pub fn field_style_reference() {}

/// Test: Covered via the `field_style_reference` tool's graceful-error test
/// today; a populated-index test is future work.
pub fn self_referential_prose() {}

/// Test: `real_test_exists` exercises `unrelated_field` / `other_symbol`.
pub fn trailing_prose_after_real_name() {}

#[cfg(test)]
mod tests {
    #[test]
    fn real_test_exists() {
        assert!(true);
    }
}
EOF
  ( cd "$tmp" && git init -q && git add -A && git -c user.email=t@t -c user.name=t commit -q -m fixture )

  local viol stale rc
  viol="$(mktemp "${TMPDIR:-/tmp}/tpself.viol.XXXXXX")"
  stale="$(mktemp "${TMPDIR:-/tmp}/tpself.stale.XXXXXX")"
  ( cd "$tmp" && scan "$viol" "$stale" )

  # Expect exactly one violation: dangling_test_missing. Nothing else —
  # not real_test_exists, not the glob/module-ref/prose-only annotation, not
  # the trailing-prose patterns (field mentions, self-referential tool
  # names, or symbols named after the real leading test name) that a naive
  # whole-blob backtick scan would misfire on.
  if [ "$(wc -l < "$viol" | tr -d ' ')" != "1" ]; then
    echo "self-test FAIL: expected exactly 1 violation, got:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -q "dangling_test_missing" "$viol"; then
    echo "self-test FAIL: expected violation to cite dangling_test_missing, got:" >&2
    cat "$viol" >&2
    ok=0
  elif grep -qE "real_test_exists|field_name|Widget|hint_|self_referential|unrelated_field|other_symbol" "$viol"; then
    echo "self-test FAIL: a trailing-prose symbol was incorrectly flagged as dangling:" >&2
    cat "$viol" >&2
    ok=0
  fi
  rm -f "$viol" "$stale"

  if [ "$ok" -eq 1 ]; then
    echo "check_test_pointers self-test: OK (valid pointer passes, dangling pointer caught, prose/glob/module-ref ignored)."
    return 0
  fi
  return 1
}

# ===========================================================================
# Entry point
# ===========================================================================
MODE="check"
for arg in "$@"; do
  case "$arg" in
    --update) MODE="update" ;;
    --seed) MODE="seed" ;;
    --self-test) MODE="self-test" ;;
    -h|--help)
      grep '^#' "$SCRIPT_PATH" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_test_pointers: unknown argument: $arg" >&2
      echo "usage: check_test_pointers.sh [--update | --seed | --self-test]" >&2
      exit 2
      ;;
  esac
done

if [ "$MODE" = "self-test" ]; then
  run_self_test
  exit $?
fi

REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"
ALLOWLIST="$REPO_ROOT/.test-pointer-allowlist.tsv"

VIOL="$(mktemp "${TMPDIR:-/tmp}/tpviol.XXXXXX")"
STALE="$(mktemp "${TMPDIR:-/tmp}/tpstale.XXXXXX")"
trap 'rm -f "$VIOL" "$STALE"' EXIT

scan "$VIOL" "$STALE"

if [ "$MODE" = "seed" ]; then
  # Bulk-grandfather every CURRENT dangling pointer (mirrors
  # check_line_cap.sh --seed: the one-time bootstrap for an existing
  # codebase that has never been mechanically checked before). Never used
  # for ordinary maintenance — new dangling pointers introduced after the
  # gate lands must be fixed, not seeded. Refuses to run if the allowlist
  # already has grandfathered rows, to avoid accidentally re-seeding over
  # hand-curated entries; use --update to prune, or edit the TSV directly.
  existing_rows=0
  if [ -f "$ALLOWLIST" ]; then
    existing_rows="$(awk -F'\t' '$0 !~ /^#/ && NF>=3' "$ALLOWLIST" | wc -l | tr -d ' ')"
  fi
  if [ "${existing_rows:-0}" -gt 0 ]; then
    echo "check_test_pointers --seed: refusing — $ALLOWLIST already has ${existing_rows} row(s). Remove it first (or edit by hand) if you really intend to re-seed." >&2
    exit 1
  fi
  {
    echo "# .test-pointer-allowlist.tsv — grandfathered dangling Test: doc-comment"
    echo "# pointers (issue #2458)."
    echo "# Format: <relative/path><TAB><line><TAB><cited-test-name>  (one row per"
    echo "# grandfathered dangling pointer)."
    echo "# A row is grandfathered ONLY when fixing it genuinely requires writing a"
    echo "# new test first — prefer fixing the doc comment (correct the name, or drop"
    echo "# the reference) over adding an entry here."
    echo "# Ratchet: entries may only be REMOVED once the pointer is fixed (or the"
    echo "# named test is written); the gate FAILS on any entry that is no longer"
    echo "# dangling, forcing prompt removal. Prune with: scripts/check_test_pointers.sh --update"
    echo "#"
    echo "# Seeded $(date -u +%Y-%m-%d) by --seed: first-ever run of this gate found"
    echo "# this many pre-existing dangling pointers across a workspace where the"
    echo "# Test: convention was never mechanically checked before. New pointers"
    echo "# introduced from this point on are NOT grandfathered — fix them for real."
    awk -F'\t' '{print $1"\t"$2"\t"$3}' "$VIOL" | sort -u
  } > "$ALLOWLIST"
  seeded="$(wc -l < "$VIOL" | tr -d ' ')"
  echo "check_test_pointers --seed: wrote $ALLOWLIST with ${seeded} grandfathered row(s)."
  exit 0
fi

if [ "$MODE" = "update" ]; then
  if [ ! -f "$ALLOWLIST" ] || [ ! -s "$STALE" ]; then
    echo "check_test_pointers --update: no stale allowlist entries to prune."
    exit 0
  fi
  NEW="$(mktemp "${TMPDIR:-/tmp}/tpnew.XXXXXX")"
  awk -F'\t' -v stalefile="$STALE" '
    BEGIN {
      while ((getline line < stalefile) > 0) { stale[line] = 1 }
    }
    /^#/ { print; next }
    NF < 3 { print; next }
    { key = $1"\t"$2"\t"$3; if (!(key in stale)) print }
  ' "$ALLOWLIST" > "$NEW"
  mv "$NEW" "$ALLOWLIST"
  pruned="$(wc -l < "$STALE" | tr -d ' ')"
  echo "check_test_pointers --update: pruned ${pruned} stale entr$([ "$pruned" = "1" ] && echo y || echo ies) from $ALLOWLIST."
  exit 0
fi

# ----- check mode -----
violations=0
if [ -s "$VIOL" ]; then
  while IFS=$'\t' read -r p l n c; do
    [ -n "$p" ] || continue
    echo "FAIL: ${p}:${l}: Test: pointer cites \`${n}\` — no \`fn ${n}(\` found in crate \`${c}\`." >&2
    violations=$((violations + 1))
  done < "$VIOL"
fi

stale=0
if [ -s "$STALE" ]; then
  while IFS=$'\t' read -r p l n; do
    [ -n "$p" ] || continue
    echo "FAIL: ${p}:${l}: \`${n}\` is allowlisted in .test-pointer-allowlist.tsv but is no longer dangling — remove its entry (run --update)." >&2
    stale=$((stale + 1))
  done < "$STALE"
fi

total=$((violations + stale))
if [ "$total" -gt 0 ]; then
  echo "test-pointers: ${violations} dangling pointer(s), ${stale} stale allowlist entr$([ "$stale" = "1" ] && echo y || echo ies) — FAILED." >&2
  exit 1
fi

echo "test-pointers: 0 dangling pointers — OK."
exit 0
