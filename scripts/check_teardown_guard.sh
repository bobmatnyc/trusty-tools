#!/usr/bin/env bash
#
# check_teardown_guard.sh — every durable-write call site in trusty-search either
# holds the per-index teardown guard in its own scope, or is DECLARED as not
# holding it, with a reason (issue #3049 round 4).
#
# Why: `DELETE /indexes/:id?delete_data=true` quiesces by taking the EXCLUSIVE
#   side of that index's teardown lock; every writer is supposed to hold the
#   SHARED side for the span of its write. Which paths those are was tracked in a
#   prose table in `crates/trusty-search/src/service/reindex/semaphore.rs`, hand-
#   derived by grepping method names. That table was wrong three review rounds
#   running:
#     - round 1 quiesced on the wrong primitive and missed 2 writers;
#     - round 2 rewrote the table and missed the `PATCH /config` catch-up;
#     - round 3 fixed that, declared the table complete, and missed the startup
#       schema migration — which lives in `src/commands`, outside the `src/service`
#       subtree the table was grepped from, inside a `tokio::spawn`;
#     - all three missed `watch_loop::handle_modified`'s stale-chunk removal,
#       because `remove_chunk` was never on the hand-written grep list.
#   Each miss let a delete see an uncontended lock, report `quiesced: true`, and
#   `remove_dir_all` a directory a live writer was writing into. A table that is
#   re-derived by hand each round is not a mechanism. This is.
#
# What: for every tracked non-test `.rs` file under `crates/trusty-search/src/`,
#   finds each call to a method listed in `scripts/teardown-guard-methods.tsv`,
#   and asks whether a teardown-guard acquisition
#   (`acquire_index_teardown_read` / `_write`) appears EARLIER IN THE SAME SCOPE.
#
#   "Scope" is the innermost enclosing `fn` body OR spawned-task body
#   whichever is inner. A DEFERRED BODY is a spawn call (`tokio::spawn(…)`,
#   `spawn_blocking(…)`, `spawn_local(…)`), an `async` / `async move` block, or a
#   closure with a braced body.
#
#   Treating a deferred body as its own scope is what makes the missed shapes
#   visible: the guard is a scope guard, DROPPED when the function that took it
#   returns, so it does not cover work that function deferred past its own
#   return — the exact `commands/start/daemon.rs` shape. Requiring the guard
#   EARLIER than the call is what makes an ordering bug visible — the exact
#   `watch_loop::handle_modified` shape, where the guard was real but sat after a
#   `remove_chunk` loop that had already deleted from redb.
#
#   Matching only `spawn(` was not enough, and #3049 round 4 review proved it by
#   defeating the first version of this gate with idiomatic Rust:
#
#     let fut = async move { idx.commit_parsed_batch(…).await };
#     tokio::spawn(fut);
#
#   The write shares neither a line nor a brace with the `spawn(`, and the block
#   opens and closes on one line so its net brace delta is zero — the scope stack
#   never saw it, and the gate blamed the enclosing guard, exit 0. Hence two
#   rules rather than one: any deferred body opens a scope, AND a call textually
#   to the right of a deferred opener on its own line is inside that body whether
#   or not the stack ever saw it. Naming a future before spawning it is ordinary
#   (tracing, instrumentation, a conditional spawn), so this was reachable.
#
#   Disclosed cost of the broader rule: a durable write inside a closure or async
#   block that is immediately AWAITED is safe but still needs a manifest row,
#   because the gate cannot see the await. One such row exists today
#   (`contrib_graph.rs`). That is the intended direction — a needless row is
#   cheap, a missed writer corrupts a corpus.
#
#   A call site that is not self-evidently guarded must have a row in
#   `scripts/teardown-guard-manifest.tsv` — the one place an exemption is
#   declared, with a mandatory reason. Two dispositions:
#     CALLER:<fn>  the guard is held by an ancestor; <fn> is named and must
#                  exist in a scanned file that itself acquires a guard.
#     EXEMPT       deliberately unguarded; the reason column says why.
#   A row that no longer matches any unguarded call site FAILS as stale, so the
#   manifest cannot quietly accumulate claims that stopped being true.
#
# FAIL-CLOSED, deliberately. Everything ambiguous fails:
#   - a call site whose enclosing scope cannot be determined is UNGUARDED;
#   - a `git ls-files` that errors is a TOOL ERROR, not an empty clean scan;
#   - scanning fewer files / finding fewer call sites / finding fewer guard
#     acquisitions than the declared floors below is a SCAN FLOOR failure. A gate
#     whose pattern silently stopped matching reports "0 violations" identically
#     to a clean tree; this repo has already shipped that bug (#4618, #5440).
#
# KNOWN LIMITS, stated rather than papered over:
#   - Brace-based scope tracking has no notion of string or char literals, so a
#     `{` or `}` inside one skews the balance for the rest of the file. The
#     failure direction is toward reporting MORE unguarded sites (wrong or
#     missing scope names), never toward silently satisfying one.
#   - `CALLER:<fn>` is verified only as "that function exists and does take a
#     guard" — the gate does not prove the call graph. It makes the claim
#     explicit and checkable by a human, which prose in a doc comment was not.
#   - The seed method list is hand-maintained (see its header). This gate closes
#     "existing method, new call site" drift, not "brand-new write method".
#   - Deferred-body detection is textual. A body reached through a level of
#     indirection the text does not show — a write inside a function that is
#     itself only ever called from a spawned task, with no manifest row saying so
#     — is attributed to wherever it is written. `CALLER:<fn>` rows are what
#     record those chains, and they are human-checked, not proven.
#
# Usage:  bash scripts/check_teardown_guard.sh
# Exit:   0 clean; 1 on any undeclared site, stale/malformed manifest row, tool
#         error, or scan-floor breach.
#
# Test: scripts/check_teardown_guard_selftest.sh (mutation self-test: proves the
#   gate rejects an empty scan AND catches an unguarded call, a guard placed
#   after the call, and a call inside a spawn whose guard is outside it).
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). POSIX tools only.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# shellcheck source=scripts/lib/sloc_awk.sh
. "$SCRIPT_DIR/lib/sloc_awk.sh"

SRC_DIR="crates/trusty-search/src"
METHODS_TSV="scripts/teardown-guard-methods.tsv"
MANIFEST_TSV="scripts/teardown-guard-manifest.tsv"

# ---------------------------------------------------------------------------
# Scan floors. Recorded when the gate was written (#3049 round 4) against the
# real tree, set just under the observed values so ordinary refactoring does not
# trip them but a pattern that stops matching does. Raise them deliberately;
# never lower one to make a red gate green.
# ---------------------------------------------------------------------------
MIN_FILES=150     # non-test .rs files scanned   (observed at #3049 rd4: 284)
MIN_SITES=15      # seed-method call sites found (observed at #3049 rd4: 23)
MIN_GUARDS=8      # guard acquisitions found     (observed at #3049 rd4: 16)

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -f "$METHODS_TSV" ] || fail "missing seed method list $METHODS_TSV"
[ -f "$MANIFEST_TSV" ] || fail "missing manifest $MANIFEST_TSV"

TMPDIR_BASE="${TMPDIR:-/tmp}"
FILELIST="$(mktemp "$TMPDIR_BASE/tdguard.files.XXXXXX")"
METHODS="$(mktemp "$TMPDIR_BASE/tdguard.methods.XXXXXX")"
SKIPF="$(mktemp "$TMPDIR_BASE/tdguard.skip.XXXXXX")"
SITES="$(mktemp "$TMPDIR_BASE/tdguard.sites.XXXXXX")"
GUARDFILES="$(mktemp "$TMPDIR_BASE/tdguard.guardfiles.XXXXXX")"
trap 'rm -f "$FILELIST" "$METHODS" "$SKIPF" "$SITES" "$GUARDFILES"' EXIT

# --- seed method list ------------------------------------------------------
awk -F'\t' '/^[[:space:]]*#/ || NF == 0 { next } $1 != "" { print $1 }' \
  "$METHODS_TSV" > "$METHODS"
METHOD_COUNT="$(awk 'END{print NR+0}' "$METHODS")"
[ "$METHOD_COUNT" -gt 0 ] || fail "seed method list $METHODS_TSV parsed to zero methods."

# --- file set --------------------------------------------------------------
# Materialised before the loop so git's exit status is observable; a
# `git ls-files | while` pipeline hides a failing git behind an empty stream
# (#4618).
if ! git ls-files "$SRC_DIR" > "$FILELIST.all"; then
  rm -f "$FILELIST.all"
  fail "TOOL ERROR — 'git ls-files $SRC_DIR' exited non-zero; the file set could
      not be enumerated, so nothing was scanned. That is not a pass (#4618)."
fi

# Test/benchmark files are excluded using the SLOC cap gate's classification
# (basename tests.rs, *_test.rs, *_tests.rs, a /tests/ or /benches/ path
# segment) PLUS one rule that gate does not have: a `tests_*.rs` / `tests*.rs`
# basename. trusty-search names its `#[cfg(test)] mod` sibling files that way
# — `service/server/tests_health.rs`, `core/indexer/tests_idle_evict.rs` — and
# the cap gate's suffix-only rule counts them as production. Scanning them here
# would demand a manifest row per test fixture, which buries the real rows in
# noise. The deviation only ever REMOVES test files from the scan; a production
# file is never named `tests*.rs`.
awk '
  /\.rs$/ {
    p = $0
    n = split(p, seg, "/")
    base = seg[n]
    if (base == "tests.rs") next
    if (base ~ /_tests?\.rs$/) next
    if (base ~ /^tests/) next
    if (p ~ /\/tests\//) next
    if (p ~ /\/benches\//) next
    print p
  }
' "$FILELIST.all" > "$FILELIST"
rm -f "$FILELIST.all"

FILE_COUNT="$(awk 'END{print NR+0}' "$FILELIST")"
if [ "$FILE_COUNT" -lt "$MIN_FILES" ]; then
  fail "SCAN FLOOR — only ${FILE_COUNT} non-test .rs file(s) under ${SRC_DIR} were
      scanned, below the declared minimum of ${MIN_FILES} (MIN_FILES in this
      script). A gate that scans nothing reports '0 violations' and cannot fail."
fi

# --- per-file analysis -----------------------------------------------------
: > "$SITES"
while IFS= read -r f; do
  [ -n "$f" ] || continue
  [ -f "$f" ] || continue
  # Inline `#[cfg(test)] mod { … }` regions are excluded via the shared SLOC
  # matcher, so unit tests inside a production file are not scanned. Whole test
  # FILES were already dropped above.
  awk -v emit_skip=1 "$SLOC_AWK" "$f" > "$SKIPF" || fail "TOOL ERROR — cfg(test) scan failed on $f"
  awk -v SKIPF="$SKIPF" -v METHODSF="$METHODS" -v PATHNAME="$f" '
    function rtrim(s) { sub(/[ \t\r]+$/, "", s); return s }
    # Text before a `//` line comment. Doc comments are where the guard function
    # is NAMED without being CALLED, so stripping them is what keeps a doc
    # reference from registering as a real acquisition.
    function decomment(s,   i) {
      i = index(s, "//")
      if (i > 0) return substr(s, 1, i - 1)
      return s
    }
    function brace_delta(s,   a, b, t) {
      t = s; a = gsub(/\{/, "", t)
      t = s; b = gsub(/\}/, "", t)
      return a - b
    }
    # A body whose execution is DEFERRED past the enclosing function: a spawned
    # task, an `async` block, or a closure. The guard is a scope guard, so it is
    # dropped when the function that took it returns — anything deferred past
    # that point is unprotected by it. Ordered so `spawn` wins on a line that is
    # both (`spawn_blocking(move || …)`), keeping scope names stable.
    function deferred_kind(s) {
      if (s ~ /spawn[A-Za-z_]*[ \t]*\(/) return "spawn"
      if (s ~ /async[ \t]+move[ \t]*\{/ || s ~ /async[ \t]*\{/) return "async"
      if (s ~ /\|[^|]*\|[ \t]*\{/ || s ~ /\|\|[ \t]*\{/) return "closure"
      return ""
    }
    # Column at which a deferred body opens on this line, 0 when none. Used to
    # place a call that shares a line with an opener on the correct side of it:
    # `indexer.remove_file(…).map_err(|e| {` opens a closure AFTER the write, so
    # the write belongs to the enclosing function, not to the closure.
    function deferred_pos(s,   best, p) {
      best = 0
      if (match(s, /spawn[A-Za-z_]*[ \t]*\(/)) best = RSTART
      if (match(s, /async[ \t]+move[ \t]*\{/) || match(s, /async[ \t]*\{/)) {
        p = RSTART; if (best == 0 || p < best) best = p
      }
      if (match(s, /\|[^|]*\|[ \t]*\{/)) { p = RSTART; if (best == 0 || p < best) best = p }
      return best
    }
    # Innermost fn name, suffixed with the innermost deferred body wrapping it.
    function scope_key(   i, defer, fname) {
      defer = ""; fname = ""
      for (i = sp; i >= 1; i--) {
        if (kind[i] == "spawn" || kind[i] == "async" || kind[i] == "closure") {
          if (defer == "") defer = kind[i]
          continue
        }
        fname = kind[i]; break
      }
      if (fname == "") fname = "<file-scope>"
      return (defer != "") ? fname "+" defer : fname
    }
    FILENAME == SKIPF   { skip[$1 + 0] = 1; next }
    FILENAME == METHODSF { methods[++nm] = $1; next }
    {
      code = (FNR in skip) ? "" : decomment($0)

      if (code ~ /(^|[^A-Za-z0-9_])fn[ \t]+[A-Za-z_][A-Za-z0-9_]*/) {
        t = code
        sub(/^.*(^|[^A-Za-z0-9_])fn[ \t]+/, "", t)
        sub(/[^A-Za-z0-9_].*$/, "", t)
        if (t != "") pending_fn = t
      }
      dk = deferred_kind(code)
      dpos = (dk != "") ? deferred_pos(code) : 0
      if (dk != "") pending_defer = dk

      d = brace_delta(code)
      pushed_here = 0
      if (d > 0 && (pending_defer != "" || pending_fn != "")) {
        # Remembered so a call to the LEFT of the opener on this line can still
        # be attributed to the scope that was current before the push.
        outer_key = scope_key()
        outer_inst = (sp >= 1) ? instid[sp] : 0
        sp++
        kind[sp] = (pending_defer != "") ? pending_defer : pending_fn
        dbefore[sp] = depth
        instid[sp] = ++counter
        pending_defer = ""
        pending_fn = ""
        pushed_here = 1
      }
      depth += d
      while (sp >= 1 && depth <= dbefore[sp]) sp--

      inst = (sp >= 1) ? instid[sp] : 0

      # A guard taken at the very line that opens a scope belongs to that scope.
      if (code ~ /acquire_index_teardown_(read|write)[ \t]*\(/) {
        if (!(inst in gseen)) gseen[inst] = 1
        print "GUARD\t" PATHNAME "\t" FNR
      }

      for (i = 1; i <= nm; i++) {
        if (match(code, "\\." methods[i] "[ \t]*\\(")) {
          call_at = RSTART
          site_inst = inst
          site_key = scope_key()
          # A call to the LEFT of the deferred opener on this line is not inside
          # the body it starts. Without this, `indexer.remove_file(…)
          # .map_err(|e| {` would be blamed on the closure.
          if (pushed_here && dpos > 0 && call_at < dpos) {
            site_inst = outer_inst
            site_key = outer_key
          }
          state = (site_inst in gseen) ? "OK" : "UNGUARDED"
          # An undeterminable scope is never self-satisfied: file scope has no
          # fn to hold a guard across, so it must be declared.
          if (site_inst == 0) state = "UNGUARDED"
          # A deferred body that OPENS AND CLOSES on one line has a net brace
          # delta of zero, so the scope stack never sees it and the call reads as
          # belonging to the enclosing function. That is how
          # `let fut = async move { idx.commit_parsed_batch(…).await };` escaped
          # the first version of this gate entirely (#3049 rd4 review). Anything
          # to the LEFT of the call that opens a deferred body puts the call in
          # it, whether or not the stack ever saw that body.
          if (dpos > 0 && call_at > dpos) state = "UNGUARDED"
          print "SITE\t" PATHNAME "\t" methods[i] "\t" site_key "\t" FNR "\t" state
        }
      }
    }
  ' "$METHODS" "$SKIPF" "$f" >> "$SITES"
done < "$FILELIST"

SITE_COUNT="$(awk -F'\t' '$1=="SITE"{n++} END{print n+0}' "$SITES")"
GUARD_COUNT="$(awk -F'\t' '$1=="GUARD"{n++} END{print n+0}' "$SITES")"

if [ "$SITE_COUNT" -lt "$MIN_SITES" ]; then
  fail "SCAN FLOOR — only ${SITE_COUNT} call site(s) of the ${METHOD_COUNT} seed
      method(s) were found, below the declared minimum of ${MIN_SITES} (MIN_SITES).
      Either the match pattern stopped working or the seed list emptied out;
      neither is a clean tree."
fi
if [ "$GUARD_COUNT" -lt "$MIN_GUARDS" ]; then
  fail "SCAN FLOOR — only ${GUARD_COUNT} teardown-guard acquisition(s) were found,
      below the declared minimum of ${MIN_GUARDS} (MIN_GUARDS). If the guards were
      genuinely removed this gate is the wrong thing to silence; if the pattern
      broke, every site would read as unguarded for the wrong reason."
fi

# --- reconcile against the manifest ----------------------------------------
# Files that acquire a guard, for CALLER:<fn> verification.
awk -F'\t' '$1=="GUARD"{print $2}' "$SITES" | sort -u > "$GUARDFILES"

RC=0
# 1. Every UNGUARDED site needs a manifest row.
while IFS=$'\t' read -r _ path method scope line state; do
  [ "$state" = "UNGUARDED" ] || continue
  row="$(awk -F'\t' -v p="$path" -v m="$method" -v s="$scope" '
    /^[[:space:]]*#/ || NF == 0 { next }
    $1 == p && $2 == m && $3 == s { print; exit }
  ' "$MANIFEST_TSV")"
  if [ -z "$row" ]; then
    echo "FAIL: UNDECLARED — $path:$line calls .$method() in scope '$scope' without" >&2
    echo "      acquiring the teardown guard earlier in that same scope, and has no row" >&2
    echo "      in $MANIFEST_TSV." >&2
    echo "      Fix it (take the guard) or declare it:" >&2
    printf '        %s\t%s\t%s\tEXEMPT\t<why this cannot corrupt a deleted index>\n' \
      "$path" "$method" "$scope" >&2
    RC=1
    continue
  fi
  disp="$(printf '%s' "$row" | awk -F'\t' '{print $4}')"
  reason="$(printf '%s' "$row" | awk -F'\t' '{print $5}')"
  if [ -z "$reason" ]; then
    echo "FAIL: NO REASON — the row for $path .$method() in '$scope' has an empty" >&2
    echo "      reason column. An exemption without a reason is an omission." >&2
    RC=1
    continue
  fi
  case "$disp" in
    EXEMPT) ;;
    CALLER:*)
      fn="${disp#CALLER:}"
      if [ -z "$fn" ]; then
        echo "FAIL: MALFORMED — $path .$method() declares CALLER: with no function name." >&2
        RC=1
        continue
      fi
      found=0
      while IFS= read -r gf; do
        # `fn <name>(` or `fn <name><` — a definition, not a call. Only files
        # that themselves acquire a guard are searched, which is what makes this
        # a check rather than a spell-check of the manifest.
        if grep -qE "fn ${fn}[(<]" "$gf" 2>/dev/null; then
          found=1
          break
        fi
      done < "$GUARDFILES"
      if [ "$found" -ne 1 ]; then
        echo "FAIL: UNVERIFIABLE CALLER — $path .$method() claims its guard is held by" >&2
        echo "      '$fn', but no scanned file that acquires a teardown guard defines it." >&2
        echo "      Either the ancestor was renamed or it stopped taking the guard." >&2
        RC=1
      fi
      ;;
    *)
      echo "FAIL: MALFORMED — unknown disposition '$disp' for $path .$method() in '$scope'." >&2
      echo "      Use EXEMPT or CALLER:<fn>." >&2
      RC=1
      ;;
  esac
done < "$SITES"

# 2. Every manifest row must still match an UNGUARDED site. A row whose site was
#    fixed, moved, or deleted is a claim that stopped being true, and a stale
#    claim is exactly how the prose table drifted.
while IFS=$'\t' read -r path method scope disp reason; do
  case "$path" in ''|\#*) continue ;; esac
  [ -n "$method" ] || continue
  hit="$(awk -F'\t' -v p="$path" -v m="$method" -v s="$scope" '
    $1=="SITE" && $2==p && $3==m && $4==s && $6=="UNGUARDED" { n++ } END { print n+0 }
  ' "$SITES")"
  if [ "$hit" -eq 0 ]; then
    echo "FAIL: STALE ROW — $MANIFEST_TSV declares $path .$method() in scope '$scope'," >&2
    echo "      but no such unguarded call site exists. If it is now guarded in-scope," >&2
    echo "      delete the row; if it moved, update it." >&2
    RC=1
  fi
done < "$MANIFEST_TSV"

if [ "$RC" -ne 0 ]; then
  echo "" >&2
  echo "Background: crates/trusty-search/src/service/reindex/semaphore.rs's" >&2
  echo "INDEX_TEARDOWN_LOCKS doc comment explains what the guard protects (#3049)." >&2
  exit 1
fi

echo "OK: teardown guard — ${SITE_COUNT} call site(s) of ${METHOD_COUNT} durable-write method(s) across ${FILE_COUNT} file(s); ${GUARD_COUNT} guard acquisition(s); every unguarded site declared in ${MANIFEST_TSV}."
