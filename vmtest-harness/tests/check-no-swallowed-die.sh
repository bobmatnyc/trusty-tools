#!/usr/bin/env bash
#
# check-no-swallowed-die.sh — the STATIC regression guard for issue #16.
#
# WHAT IT ENFORCES.  `die <code>` classifies a run by writing the write-once
# slot §2 reserves for the first classified failure.  Since #16 that slot is
# file-backed, so classification survives a fork — but three constructs discard
# the child's exit STATUS outright, and no side channel can give that back:
#
#     for x in $(f)          the for-list's status is discarded
#     done <<EOF $(f) EOF    heredoc expansion is redirection setup
#     cmd <(f)               `<( )` has no status channel at all
#
# At those, a `die` neither classifies nor aborts: the caller reads an empty
# list and carries on.  The fix is structural — DOC-2 §12.1's out-path
# convention — and this file is what stops it being undone.
#
# WHY NOT shellcheck.  Measured, not assumed.  With
# `-o check-extra-masked-returns`, SC2312 fires 108 times across this harness
# (it flags every `$(conf_get …)`, having no way to know which functions
# classify) and catches ONE of the eleven real defects — and it does not fire on
# `for x in $(f)` at all, the worst class.  A bespoke check that knows which
# functions can `die` is both quieter and stronger.
#
# IT IS SELF-DERIVING, NOT HAND-MAINTAINED.  The classifying set is recomputed
# from the source on every run: every function body is extracted, those
# containing a literal `die <n>` are marked, and the set is closed transitively
# over intra-harness calls.  A `die` added anywhere is picked up with no edit
# here — which is the property that makes this a gate rather than a snapshot.
#
# ---------------------------------------------------------------------------
# THE INDIRECTION BLIND SPOT — stated because it is the one thing this cannot
# see, and the one place three real defects hid.
#
# `install_assert_install_count` takes its accessor as a STRING parameter and
# invokes it as `"$accessor"` (lib/source.sh).  No grep for a function NAME
# finds that call, so sites 3-5 of #16's inventory were found by reading, not by
# tooling.  Stage 3 below therefore turns the blind spot into a REQUIREMENT: any
# indirect invocation must carry an explicit `swallowed-die-check:` annotation
# naming why it is safe.  The checker still cannot verify the callee — but a new
# one can no longer appear silently.
#
# TARGET: /bin/bash 3.2.57 and POSIX awk.  No associative-array bashisms, no
# `mapfile` (DOC-2 §Shell discipline).

set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HARNESS_DIR=$(cd "$TESTS_DIR/.." && pwd)
cd "$HARNESS_DIR" || exit 1

FILES="vmtest"
for f in lib/*.sh scenarios/*.sh; do
    [ -f "$f" ] && FILES="$FILES $f"
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-swallowed-die.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# ---------------------------------------------------------------------------
# Stage 1 — derive the classifying set.
#
# Every function in this harness is defined at column 0, in one of two shapes:
#   name() {            … multi-line, closed by a `}` at column 0
#   name() { … ; }      … one-line
# Both are extracted.  Comment lines are dropped before any matching so a
# function that merely NAMES `die` in prose is not counted as calling it.
# ---------------------------------------------------------------------------

awk '
function strip(l,   s) {
    s = l
    sub(/^[ \t]+/, "", s)
    if (substr(s, 1, 1) == "#") return ""
    # A trailing comment, only when the line has balanced quotes — a `#` inside
    # a string is not a comment and stripping it would corrupt the line.
    if (gsub(/"/, "\"", l) % 2 == 0 && gsub(/'"'"'/, "'"'"'", l) % 2 == 0)
        sub(/[ \t]+#[^"'"'"']*$/, "", l)
    return l
}
function record(fn, body,   n, i, toks, t) {
    if (body ~ /(^|[^A-Za-z0-9_])die[ \t]+[0-9]/) print fn > DIRECT
    n = split(body, toks, /[^A-Za-z0-9_]+/)
    for (i = 1; i <= n; i++) {
        t = toks[i]
        if (t != "" && t != fn) print fn "\t" t > EDGES
    }
}
/^[A-Za-z_][A-Za-z0-9_]*\(\)[ \t]*\{.*\}[ \t]*$/ {
    fn = $0; sub(/\(\).*/, "", fn)
    body = $0; sub(/^[^{]*\{/, "", body); sub(/\}[ \t]*$/, "", body)
    print fn > FUNCS
    record(fn, strip(body))
    next
}
/^[A-Za-z_][A-Za-z0-9_]*\(\)[ \t]*\{[ \t]*$/ {
    fn = $0; sub(/\(\).*/, "", fn)
    print fn > FUNCS
    body = ""
    while ((getline nl) > 0) {
        if (nl == "}") break
        body = body "\n" strip(nl)
    }
    record(fn, body)
    next
}
' DIRECT="$WORK/direct.txt" EDGES="$WORK/edges.txt" FUNCS="$WORK/funcs.txt" \
    $FILES

for t in direct edges funcs; do
    [ -f "$WORK/$t.txt" ] || : > "$WORK/$t.txt"
done

# Refuse a vacuous scan: a checker that examined nothing must not report green.
nfuncs=$(sort -u "$WORK/funcs.txt" | grep -c .)
ndirect=$(sort -u "$WORK/direct.txt" | grep -c .)
if [ "$nfuncs" -lt 50 ] || [ "$ndirect" -lt 20 ]; then
    printf 'check-no-swallowed-die: REFUSING a vacuous scan — extracted %s functions and %s direct `die` callers from:\n  %s\n' \
        "$nfuncs" "$ndirect" "$FILES" >&2
    exit 1
fi

# Transitive closure over intra-harness calls.
sort -u "$WORK/direct.txt" > "$WORK/classify.txt"
sort -u "$WORK/funcs.txt"  > "$WORK/known.txt"
while : ; do
    awk -F'\t' '
        NR == FNR { cls[$0] = 1; next }
        ($2 in cls) { print $1 }
    ' "$WORK/classify.txt" "$WORK/edges.txt" | sort -u > "$WORK/grown.txt"
    # Only real harness functions may enter the set — `$2` above can be any
    # token in a body, including an external command's name.
    sort -u "$WORK/classify.txt" "$WORK/grown.txt" \
        | grep -x -F -f "$WORK/known.txt" > "$WORK/next.txt"
    if cmp -s "$WORK/classify.txt" "$WORK/next.txt"; then break; fi
    mv "$WORK/next.txt" "$WORK/classify.txt"
done

nclassify=$(grep -c . "$WORK/classify.txt")
printf 'check-no-swallowed-die: %s functions, %s call `die` directly, %s classify transitively\n' \
    "$nfuncs" "$ndirect" "$nclassify"

# ---------------------------------------------------------------------------
# Stage 2 — flag any classifying name in a status-discarding context, and
# stage 3 — require an annotation on every indirect invocation.
#
# Both run over the same preprocessed stream, because both need the same three
# things and getting any of them wrong produces false positives:
#
#   - HEREDOC BODIES ARE NOT HARNESS CODE.  `n1_reachability_probe` emits a
#     whole /bin/sh script for the GUEST inside `<<'N1PROBE'`; scanning it as
#     driver code flags an interpreter path (`"$s" "$f" …`) as an indirect
#     call.  A QUOTED heredoc expands nothing at all and is skipped entirely;
#     an UNQUOTED one is scanned, because `$( )` inside it does expand — that
#     is the vacuous-PASS construct itself.
#   - BACKSLASH CONTINUATIONS ARE ONE LOGICAL LINE.  Without joining, the
#     second line of a wrapped `git status … -- "$SRC_FIX_TRACKED" …` looks
#     like a command starting with `"$VAR"`.
#   - SINGLE-QUOTED TEXT EXPANDS NOTHING.  The `die` messages added by #16
#     quote the very construct they warn about (`a $(tsv_scope_packages) runs
#     its die in a fork`), and an awk one-liner's program body is a
#     single-quoted string full of `$1`.  Neither is shell code.
# ---------------------------------------------------------------------------

awk '
# Remove single-quoted spans, which expand nothing.  Only when the quotes on
# this logical line balance — an unbalanced line is left intact rather than
# mangled, on the principle that a false positive beats a missed defect.
function dequote(l,   out, i, c, inq, n) {
    # Drop every backslash-escape pair FIRST.  Two reasons, both load-bearing:
    # the `'"'"'\'"'"''"'"'` idiom otherwise makes the quote count odd and defeats the
    # balance test below, and an escaped `` \` `` inside a diagnostic is a
    # literal, not a substitution — deleting it is what keeps the backtick ban
    # from firing on every `die` message in the harness.
    gsub(/\\./, "", l)
    n = gsub(/'"'"'/, "'"'"'", l)
    if (n % 2 != 0) return l
    out = ""; inq = 0
    for (i = 1; i <= length(l); i++) {
        c = substr(l, i, 1)
        if (c == "'"'"'") { inq = !inq; continue }
        if (!inq) out = out c
    }
    return out
}
function strip(l,   s) {
    s = l
    sub(/^[ \t]+/, "", s)
    if (substr(s, 1, 1) == "#") return ""
    return dequote(l)
}
function names_in(t,   n, i, toks) {
    n = split(t, toks, /[^A-Za-z0-9_]+/)
    for (i = 1; i <= n; i++) if (toks[i] in cls) return toks[i]
    return ""
}
# Does the balanced span opened at position p of s name a classifying function?
function span_names(s, p,   depth, i, c, inner) {
    depth = 1; inner = ""
    for (i = p; i <= length(s) && depth > 0; i++) {
        c = substr(s, i, 1)
        if (c == "(") depth++
        else if (c == ")") { depth--; if (depth == 0) break }
        inner = inner c
    }
    return names_in(inner)
}
function report(f, ln, kind, name, text) {
    printf "%s:%d: [%s] `%s` classifies, and its status is discarded here\n", f, ln, kind, name
    printf "       %s\n", text
    bad++
}
function scan(f, ln, raw, inhd,   line, i, c, pre, nm, isfor, j, s) {
    line = strip(raw)
    if (line == "") return
    # BACKTICKS ARE BANNED OUTRIGHT in this harness, and the ban is a #16
    # control rather than a style rule.  `` `f` `` is a command substitution
    # exactly as `$(f)` is, so `` for x in `tsv_scope_packages` `` reintroduces
    # the whole pre-fix defect — and NOTHING would catch it: the scan below
    # matches `$(` and `<(` only, and shellcheck SC2006 is severity `style`,
    # below the `-S error` this repo pins.  Banning beats scanning because
    # backticks do not nest and cannot be parsed with the paren-balancing the
    # rest of this check relies on.  There are zero backticks to convert: every
    # one in the harness today is a BACKSLASH-ESCAPED literal inside a
    # double-quoted diagnostic, which this test does not match.
    if (line ~ /(^|[^\\])`/) {
        printf "%s:%d: [backtick-substitution] backticks are banned in this harness; use $( ).\n", f, ln
        printf "       %s\n", line
        printf "       A backtick hides a subshell from this check AND from shellcheck -S error (SC2006 is `style`).\n"
        bad++
    }
    isfor = (line ~ /^[ \t]*for[ \t]+[A-Za-z_][A-Za-z0-9_]*[ \t]+in[ \t]/)
    for (i = 1; i < length(line); i++) {
        c = substr(line, i, 2)
        if (c == "<(") {
            nm = span_names(line, i + 2)
            if (nm != "") report(f, ln, "process-substitution", nm, line)
        } else if (c == "$(") {
            nm = span_names(line, i + 2)
            if (nm == "") continue
            if (inhd)  { report(f, ln, "heredoc-substitution", nm, line); continue }
            if (isfor) { report(f, ln, "for-list", nm, line); continue }
            # A BARE assignment (`x=$(f)` / `local x=$(f)`) propagates the child
            # status to `set -e`, and since #16 the classification survives the
            # fork through the side channel.  Everything else is an argument
            # position, where the ENCLOSING command owns the status.
            pre = substr(line, 1, i - 1)
            if (pre ~ /^[ \t]*(local[ \t]+)?[A-Za-z_][A-Za-z0-9_]*=$/) continue
            report(f, ln, "argument-position", nm, line)
        }
    }
    # `run_watchdog <timeout> <logfile> <cmd> …` BACKGROUNDS its command
    # (`"$@" >"$logfile" 2>&1 &`), a subshell by construction — the hazard the
    # driver already names as its reason not to wrap a scenario in one.  No call
    # site hands it a classifying function today; this is what keeps that true.
    if (line ~ /(^|[^A-Za-z0-9_])run_watchdog[ \t]/) {
        j = index(line, "run_watchdog")
        nm = names_in(substr(line, j + 12))
        if (nm != "") report(f, ln, "run-watchdog-argument", nm, line)
    }
    # Stage 3 — the indirection blind spot, made mechanical.  `"$var"` at a
    # command position invokes a callee no name-grep can resolve, so each one
    # must carry `# swallowed-die-check: indirect — <why>` on the same or the
    # preceding line.  That is not a suppression: it is the record that a human
    # checked the one thing the tool cannot, at the site where three real
    # defects hid.
    s = line; sub(/^[ \t]*/, "", s)
    if (s ~ /^"\$[A-Za-z_][A-Za-z0-9_]*"([ \t]|$)/ \
        || line ~ /(\$\(|<\(|&&|\|\||\||;)[ \t]*"\$[A-Za-z_][A-Za-z0-9_]*"([ \t]|$)/) {
        # An annotation governs the next CODE line, however many comment lines
        # sit between them — the reason for one of these is rarely one line.
        if (!annpend) {
            printf "%s:%d: [indirect-invocation] a callee invoked through a variable; stage 2 cannot see it.\n", f, ln
            printf "       %s\n", s
            printf "       Add `# swallowed-die-check: indirect — <why this is safe>` on this or the preceding line.\n"
            bad++
        }
    }
    annpend = 0
}
NR == FNR { cls[$0] = 1; next }
FNR == 1 { inhd = 0; term = ""; quoted = 0; pend = ""; pendln = 0; annpend = 0 }
/swallowed-die-check:[ \t]*indirect/ { annpend = 1 }
{
    if (inhd) {
        t = $0; sub(/^[ \t]*/, "", t)
        if (t == term) { inhd = 0; next }
        if (!quoted) scan(FILENAME, FNR, $0, 1)
        next
    }
    if (pend == "") pendln = FNR
    l = $0
    if (l ~ /\\$/) { sub(/\\$/, "", l); pend = pend l; next }
    l = pend l; pend = ""

    scan(FILENAME, pendln, l, 0)

    # A heredoc opened on this logical line: `<<WORD`, `<<-WORD`, `<<'"'"'WORD'"'"'`.
    if (l ~ /<<-?[ \t]*['"'"'"]?[A-Za-z_][A-Za-z0-9_]*/) {
        h = l; sub(/^.*<<-?[ \t]*/, "", h)
        quoted = (substr(h, 1, 1) == "'"'"'" || substr(h, 1, 1) == "\"")
        gsub(/['"'"'"]/, "", h); sub(/[^A-Za-z0-9_].*$/, "", h)
        if (h != "") { inhd = 1; term = h }
    }
}
END { exit (bad > 0 ? 1 : 0) }
' "$WORK/classify.txt" $FILES > "$WORK/findings.txt"
findings=$?

cat "$WORK/findings.txt"

if [ "$findings" -ne 0 ]; then
    printf '\ncheck-no-swallowed-die: FAILED — %s finding(s).\n' \
        "$(grep -c '^[^ ][^:]*:[0-9][0-9]*:' "$WORK/findings.txt")" >&2
    printf 'Route the value through an out_path argument (DOC-2 §12.1), or call the function plainly.\n' >&2
    printf 'See issue #16 and `tests/subshell-classification.sh`.\n' >&2
    exit 1
fi

printf 'check-no-swallowed-die: OK — no classifying function sits in a status-discarding context\n'
