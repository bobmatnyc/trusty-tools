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
# ONE fragment holds ONE category. A bare category word on its own unindented
# line anywhere after line 1 is rejected, because it would otherwise render as
# body text under the line-1 heading — see stray_category_lines() below.
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
#   --merge     fold the pending fragments INTO an existing `## [<version>]`
#               section instead of inserting a second one (issue #5298)
#   --stdout    render the section to stdout as `## [Unreleased]` for preview —
#               no writes, no deletions
#
# Idempotency: a successful default run leaves changelog.d/ holding only its
# tracked README.md placeholder, so a second run inserts nothing rather than an
# empty section. A run refuses outright when the target version heading is
# already present.
#
# STRANDED FRAGMENTS (issue #5298). A `## [<version>]` section can already exist
# for the version being cut, because assembling is not coupled to publishing: PR
# #4824 wrote `## [1.3.5]` into trusty-mpm and `## [0.5.0]` into
# trusty-agents-common for a cut that never published, consuming 40 fragments in
# the act. Over the next six days 77 more fragments accumulated behind those
# sections. The default refusal below is correct and loud, but on its own it is a
# dead end: it names neither the fragments now stranded nor a way forward, and
# hand-splicing CHANGELOG.md is banned by repo policy. Recovery took a manual
# reverse-apply of #4824's changelog write.
#
# So the refusal now enumerates every pending fragment, and `--merge` makes the
# recovery a supported operation: new bullets are appended to the matching
# `### <Category>` subsections of the existing section, missing categories are
# inserted in CATEGORY_ORDER position, and the consumed fragments are deleted in
# the same operation. Merge stays OPT-IN rather than the default because it
# rewrites a section that may already be published and tagged — this script
# cannot tell a phantom cut from a shipped one, so that call stays with the
# operator. What is NOT acceptable either way is fragments going nowhere without
# a nonzero exit.
#
# An empty changelog.d/ is NOT an error — it is the steady state after a release,
# and a crate with no user-visible change since its last release has nothing to
# record. What guarantees a real change is recorded is the CI gate
# (scripts/check_changelog_fragment.sh), at the PR that makes the change.
#
# Test: `bash scripts/assemble_changelog_selftest.sh` — fixture-driven coverage of
# every validation branch (multi-category fragment, unknown category, bodyless,
# empty, nested, and the bullet-starting-with-a-category-word false-positive
# guard) against a throwaway workspace copy, plus the #5298 stale-section cases
# (refusal names the stranded fragments; `--merge` appends to an existing
# category, inserts a missing one in order, never duplicates a heading, and
# rejects a fragment body line that forges the plan-file marker).
# `bash -n` for syntax. Functionally,
# `scripts/assemble-changelog.sh <crate-dir> --stdout` renders the pending
# fragments without touching any file, and `--check` also validates the target
# CHANGELOG.md state (leftover `## [Unreleased]`, version already released).
#
# Usage:
#   scripts/assemble-changelog.sh <crate-dir> <version> [--check|--merge]
#   scripts/assemble-changelog.sh <crate-dir> --stdout
#
# Example:
#   scripts/assemble-changelog.sh trusty-mpm 1.3.2
#   scripts/assemble-changelog.sh trusty-mpm 1.3.5 --merge
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

# The --merge plan file's category-boundary marker: this token, a tab, then the
# category name. Defined once so the writer (render_merge_plan), the reader (the
# awk in merge_into_section) and the fragment guard (plan_marker_lines) cannot
# drift apart — a guard that stopped matching the marker it protects would be
# worse than no guard.
PLAN_MARKER="@@CATEGORY"

usage() {
  echo "Usage: scripts/assemble-changelog.sh <crate-dir> <version> [--check|--merge]" >&2
  echo "       scripts/assemble-changelog.sh <crate-dir> --stdout" >&2
  echo "" >&2
  echo "  <crate-dir>   directory under crates/ (e.g. trusty-mpm)" >&2
  echo "  <version>     released version for the new heading (e.g. 1.3.2)" >&2
  echo "  --check       validate fragments only; write nothing, delete nothing" >&2
  echo "  --merge       fold the fragments into an EXISTING [<version>] section" >&2
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

# Why: line 1 is the ONLY category line; everything after it is copied through
# verbatim. So a fragment that stacks several categories into one file —
#   Removed
#   - …
#   Changed
#   - …
# is not rejected by the category check (line 1 is valid) and not rejected by the
# bullet check (there are bullets): the bare `Changed` renders as body text and
# every bullet lands under `### Removed`. That is silent mis-rendering into
# CHANGELOG.md, permanent once released, and it really happened —
# `crates/trusty-mpm/changelog.d/4286-retire-trusty-mpm-override-files.md` packed
# four categories into one file during the 1.3.3 release and only a human diffing
# the `--stdout` preview caught it. Nothing in the tooling would have.
# What: prints "<file-line-number>\t<text>" for every line after the category
# line that reads as a SECOND category heading. Two forms are caught:
#   - a bare category word standing alone, unindented and un-bulleted
#   - a category as a markdown heading — `## Changed`, `### Fixed ###`
# The heading form is the same defect wearing different markup, and structurally
# worse: a stray `## …` inside a release section splits it for anything that
# parses on `^## ` boundaries, including this repo's cliff.toml-driven GitHub
# Release body step.
#
# Three things are deliberately NOT flagged, because they are legitimate content:
#   - anything inside a fenced code block. A fragment may show example output or
#     document the fragment format itself, and that example may contain a bare
#     `Fixed`. Fence state is tracked across ``` and ~~~, runs longer than three,
#     and info strings (```text). Whole-line matching that ignored code fences is
#     what made the seeded-CLAUDE.md-stub bug in #4286 — the third instance of
#     that shape in this codebase. A validator that rejects a fragment for
#     documenting its own format punishes thoroughness.
#   - a bullet, whatever it says: `- Changed the default timeout` is prose.
#   - an indented line: `  Changed` is authored continuation text.
#
# KNOWN LIMITATION, deliberate: an UNINDENTED hard-wrapped continuation line
# consisting solely of a category word is still flagged. Suppressing it needs a
# "preceded by a blank line" rule, and that reopens the very defect this guard
# closes — a fragment written without blank lines between its categories would
# sail through. A false positive costs one indent; a false negative ships a
# mis-rendered CHANGELOG.md permanently. The error text states the remedy.
stray_category_lines() {
  local path="$1"
  awk -v cats="${CATEGORY_ORDER}" '
    BEGIN {
      n = split(tolower(cats), list, " ")
      for (i = 1; i <= n; i++) known[list[i]] = 1
      fence = ""                                         # open fence marker, "" when closed
    }
    {
      line = $0
      sub(/\r$/, "", line)
      probe = line
      sub(/^[[:space:]]+/, "", probe)
    }
    !seen && line ~ /[^[:space:]]/ { seen = 1; next }   # the category line
    !seen { next }                                       # leading blank lines
    # Fence open/close. Counted char-by-char rather than with a /`{3,}/ interval,
    # which the classic awks this script must run under do not all support.
    probe ~ /^```/ || probe ~ /^~~~/ {
      ch = substr(probe, 1, 1)
      len = 1
      while (substr(probe, len + 1, 1) == ch) len++
      rest = substr(probe, len + 1)
      if (fence == "") {
        fence = substr(probe, 1, len)                    # info string may follow
      } else if (substr(fence, 1, 1) == ch && len >= length(fence) && rest ~ /^[[:space:]]*$/) {
        fence = ""                                       # closer: same char, no info string
      }
      next
    }
    fence != "" { next }                                 # inside a fence: content, never a category
    probe ~ /^[-*+]/ { next }                            # a bullet, whatever it says
    probe ~ /^#/ {                                       # heading form
      head = probe
      hashes = 0
      while (substr(head, hashes + 1, 1) == "#") hashes++
      if (hashes > 6) next                               # 7+ hashes is not a heading
      head = substr(head, hashes + 1)
      sub(/^[[:space:]]+/, "", head)
      sub(/[[:space:]]*#*[[:space:]]*$/, "", head)       # closing ATX hashes
      if (tolower(head) in known) {
        text = probe
        sub(/[[:space:]]+$/, "", text)
        printf "%d\t%s\n", NR, text
      }
      next
    }
    line ~ /^[[:space:]]/ { next }                       # continuation / sub-bullet
    {
      text = line
      sub(/[[:space:]]+$/, "", text)
      if (tolower(text) in known) printf "%d\t%s\n", NR, text
    }
  ' "${path}"
}

# Why: --merge hands fragment bodies to awk in a plan file whose category
# boundaries are lines beginning with PLAN_MARKER + a tab. A body line that
# begins with that same 11-byte sequence is read as a boundary, so every bullet
# below it is filed under the smuggled category instead of the fragment's own —
# and --merge then DELETES the fragment, which is the only other copy.
#
# The placed-vs-expected set check in merge_fragments() does not catch this on
# its own: when the smuggled name is a category that is ALSO independently
# pending, the placed set and the expected set are the same either way. The
# misfiled bullets are written and the fragments removed, exit 0.
#
# Rejected in EVERY mode, not only --merge, because whether a given release runs
# --merge is unknowable when the fragment is authored; catching it at --stdout
# and --check costs the author one edit, catching it at release time costs the
# operator their inputs. Rejected inside code fences too — unlike
# stray_category_lines(), which may exempt fences because a HUMAN reads the
# rendered markdown, the plan reader is line-based awk that does not track fence
# state, so a fenced marker collides exactly as hard as a bare one.
#
# What: prints "<file-line-number>\t<text>" for every line after the category
# line that begins with PLAN_MARKER + a tab. A line where the token is followed
# by a space, or one where it appears anywhere but column 0, is not the marker
# and is not flagged.
# Test: the `plan-marker-*` cases in scripts/assemble_changelog_selftest.sh.
plan_marker_lines() {
  local path="$1"
  awk -v marker="${PLAN_MARKER}" '
    BEGIN { mark = marker "\t"; n = length(mark) }
    {
      line = $0
      sub(/\r$/, "", line)
    }
    !seen && line ~ /[^[:space:]]/ { seen = 1; next }   # the category line
    !seen { next }                                       # leading blank lines
    substr(line, 1, n) == mark {
      text = line
      sub(/[[:space:]]+$/, "", text)
      # Render the tab as <TAB>. It is the whole reason the line is the marker
      # rather than ordinary prose, and a raw tab is indistinguishable from
      # spaces in a terminal — it would also split the caller`s tab-delimited
      # report field and swallow the smuggled category name.
      gsub(/\t/, "<TAB>", text)
      printf "%d\t%s\n", NR, text
    }
  ' "${path}"
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
  # Herestring, NOT `printf ... | grep -q`. Under `set -o pipefail` that pipeline
  # is a SIGPIPE race: `grep -q` exits the instant it matches, and when the body
  # is larger than the pipe buffer the still-writing `printf` takes EPIPE. With
  # pipefail the pipeline then reports that failure, so a fragment whose FIRST
  # body line is a bullet — the common shape — is rejected as having none. It
  # bit `trusty-agents/changelog.d/4476-migrated-added.md` (60 KB, bullet on
  # line 1) on Linux CI while passing on macOS, which is exactly the
  # platform-dependent flake this gate exists to prevent.
  if ! grep -qE '^-[[:space:]]' <<<"${body}"; then
    echo "ERROR: ${base} has a category but no bullet — expected at least one" >&2
    echo "       line starting with '- ' after the category line." >&2
    return 1
  fi

  local marked
  marked="$(plan_marker_lines "${path}")"
  if [[ -n "${marked}" ]]; then
    echo "ERROR: ${base} has a line that collides with the --merge plan-file" >&2
    echo "       marker '${PLAN_MARKER}' + tab:" >&2
    printf '%s\n' "${marked}" \
      | awk -F'\t' -v b="${base}" '{ printf "         %s:%s: %s\n", b, $1, $2 }' >&2
    echo "       A --merge run would read that line as a category boundary and file" >&2
    echo "       every bullet below it under the smuggled category, then DELETE this" >&2
    echo "       fragment. Rejected in every mode, and inside code fences too — the" >&2
    echo "       plan reader is line-based and does not track fences." >&2
    echo "       Indent the line by two spaces, or break up the token." >&2
    return 1
  fi

  local stray
  stray="$(stray_category_lines "${path}")"
  if [[ -n "${stray}" ]]; then
    echo "ERROR: ${base} carries a second category inside its bullet body." >&2
    echo "       Only line 1 is a category; everything after it is copied through" >&2
    echo "       VERBATIM, so these lines would render as body text under" >&2
    echo "       '### ${cat}' instead of headings of their own:" >&2
    printf '%s\n' "${stray}" \
      | awk -F'\t' -v b="${base}" '{ printf "         %s:%s: %s\n", b, $1, $2 }' >&2
    echo "       One fragment holds ONE category. Split it — the filename number" >&2
    echo "       may repeat, only the whole name must be unique:" >&2
    echo "         changelog.d/<number>-<slug>-removed.md" >&2
    echo "         changelog.d/<number>-<slug>-changed.md" >&2
    echo "       If that line is really wrapped prose and not a heading, indent it" >&2
    echo "       by two spaces (continuation text) or put it inside a code fence." >&2
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

# Why: the fresh-section renderer and the --merge planner must group fragments
# identically, or a merged section would drift in shape from an assembled one.
# What: prints the concatenated verbatim bodies of every fragment whose category
# is $1, in the order the caller already sorted them, or nothing.
category_body() {
  local cat="$1" parsed="$2" group path
  group="$(printf '%s\n' "${parsed}" | awk -F'\t' -v c="${cat}" '$1 == c { print $2 }')"
  [[ -z "${group}" ]] && return 0
  while IFS= read -r path; do
    [[ -z "${path}" ]] && continue
    fragment_body "${path}"
  done <<<"${group}"
}

# Why: keep rendering in one place so --stdout preview and the real write can
# never diverge.
# What: prints the assembled section (heading + grouped bullets) for the given
# heading text, reading the tab-separated "<category>\t<path>" list in $2.
render_section() {
  local heading="$1" parsed="$2" cat body first_group=1
  printf '%s\n' "${heading}"
  for cat in ${CATEGORY_ORDER}; do
    body="$(category_body "${cat}" "${parsed}")"
    [[ -z "${body}" ]] && continue
    printf '\n### %s\n\n' "${cat}"
    first_group=0
    printf '%s\n' "${body}"
  done
  if [[ "${first_group}" -eq 1 ]]; then
    echo "ERROR: no category groups rendered — refusing to write an empty section." >&2
    return 1
  fi
}

# Why: awk cannot take a multi-line value per category through -v, so --merge
# hands it the new bullets as a file. A fragment body cannot forge a boundary
# here — parse_fragment() has already rejected any body line carrying the marker.
# What: prints the plan — a `<PLAN_MARKER><TAB><name>` line followed by that
# category's verbatim bullet lines, for every category with pending fragments.
render_merge_plan() {
  local parsed="$1" cat body
  for cat in ${CATEGORY_ORDER}; do
    body="$(category_body "${cat}" "${parsed}")"
    [[ -z "${body}" ]] && continue
    printf '%s\t%s\n' "${PLAN_MARKER}" "${cat}"
    printf '%s\n' "${body}"
  done
}

# Why: --merge must place every pending bullet inside the EXISTING
# `## [<version>]` section without duplicating a `### <Category>` heading and
# without disturbing any other section. Insertion position matters: a new
# category belongs where CATEGORY_ORDER puts it, and a category the section
# already carries must be APPENDED to, not restated.
#
# What: rewrites $1 into $4, folding the plan file $3 into the section headed by
# `## [$2]`. Three passes over two files — the plan builds the per-category
# bullet map, changelog pass 1 records which `### <Category>` headings the
# target section already has, changelog pass 2 emits the rewrite. Every category
# it actually places is recorded in the report file $5 as `PLACED<TAB><name>`,
# which is what lets the caller prove nothing was dropped before it deletes a
# single fragment.
#
# Heading matching is `index($0, want) == 1`, not a regex: the version is
# operator-supplied and would otherwise need escaping twice (once for the shell,
# once through awk -v's own escape processing), and `## [1.3.5]` as a literal
# prefix already matches `## [1.3.5] — 2026-08-04` exactly as intended.
#
# Test: the #5298 cases in scripts/assemble_changelog_selftest.sh.
merge_into_section() {
  local changelog="$1" version="$2" plan="$3" out="$4" report="$5"
  : >"${report}"
  awk -v planfile="${plan}" -v want="## [${version}]" -v cats="${CATEGORY_ORDER}" \
    -v repfile="${report}" -v marker="${PLAN_MARKER}" '
    function record(c) { done[c] = 1; printf "PLACED\t%s\n", c > repfile }
    function emit_new(c) {
      print ""
      print "### " c
      print ""
      printf "%s", pend[c]
      record(c)
    }
    # Emits the subsection just walked (heading, its authored body with the
    # blank padding normalised, then any pending bullets for that category).
    # cur == "" is the pre-heading remainder of the section, emitted verbatim.
    function finalize(   i, first, last) {
      first = 1
      last = nb
      while (first <= last && body[first] ~ /^[[:space:]]*$/) first++
      while (last >= first && body[last] ~ /^[[:space:]]*$/) last--
      if (cur == "") {
        if (last >= first) {
          print ""
          for (i = first; i <= last; i++) print body[i]
        }
      } else {
        print ""
        print "### " cur
        print ""
        for (i = first; i <= last; i++) print body[i]
        if ((cur in pend) && !(cur in done)) {
          printf "%s", pend[cur]
          record(cur)
        }
      }
      nb = 0
      cur = ""
    }
    # A pending category with no heading of its own goes in ahead of the first
    # existing heading that outranks it. One the section already carries is
    # skipped here and appended at its own heading by finalize().
    function flush_before(h,   i, c) {
      if (!(h in rank)) return
      for (i = 1; i <= nc; i++) {
        c = order[i]
        if (rank[c] >= rank[h]) return
        if ((c in pend) && !(c in done) && !(c in hashead)) emit_new(c)
      }
    }
    function flush_rest(   i, c) {
      for (i = 1; i <= nc; i++) {
        c = order[i]
        if ((c in pend) && !(c in done)) emit_new(c)
      }
    }
    function heading_text(line,   h) {
      h = line
      sub(/^#+[[:space:]]*/, "", h)
      sub(/[[:space:]]*#*[[:space:]]*$/, "", h)
      return h
    }
    BEGIN {
      nc = split(cats, order, " ")
      for (i = 1; i <= nc; i++) rank[order[i]] = i
      mark = marker "\t"
      marklen = length(mark)
      phase = 0
      cur = ""
      nb = 0
      pass = 0
    }
    FILENAME == planfile {
      if (substr($0, 1, marklen) == mark) { pc = substr($0, marklen + 1); next }
      if (pc != "") pend[pc] = pend[pc] $0 "\n"
      next
    }
    FNR == 1 { pass++ }
    pass == 1 {
      if (index($0, want) == 1) { scan = 1; next }
      if (scan && index($0, "## ") == 1) scan = 0
      if (scan && index($0, "### ") == 1) hashead[heading_text($0)] = 1
      next
    }
    phase == 0 {
      print
      if (index($0, want) == 1) { phase = 1; cur = ""; nb = 0 }
      next
    }
    phase == 2 { print; next }
    index($0, "## ") == 1 {
      finalize()
      flush_rest()
      print ""
      print
      phase = 2
      next
    }
    index($0, "### ") == 1 {
      h = heading_text($0)
      finalize()
      flush_before(h)
      cur = h
      nb = 0
      next
    }
    { body[++nb] = $0; next }
    END {
      if (phase == 1) { finalize(); flush_rest() }
    }
  ' "${plan}" "${changelog}" "${changelog}" >"${out}"
}

# Why: --merge is the one path that rewrites text a human already reviewed, so
# it owes a proof that the rewrite placed everything before it deletes the only
# other copy of those bullets. Deleting fragments on the strength of "the awk
# exited 0" is exactly the fail-open shape #5298 is about.
# What: renders the plan, merges into a temp file, and requires the merger's
# PLACED report to list every category that had pending fragments. Any shortfall
# leaves CHANGELOG.md and changelog.d/ untouched and exits nonzero.
# Test: the #5298 `--merge` cases in scripts/assemble_changelog_selftest.sh.
# Why: both write paths end with `rm -f "${fragments[@]}"` AFTER CHANGELOG.md has
# already been replaced, and a bare `rm` there is a silent-partial arm. If it
# fails — a read-only changelog.d/, a permissions change, an immutable flag —
# `set -e` aborts before the success message, so the operator sees NO output at
# all: the section is written, the fragments are still pending, and nothing says
# so. The next run then re-adds those bullets. --merge is the more damaging of
# the two, because its re-run appends the survivor to a section that already
# contains it; the default-write path's re-run at least hits the
# already-has-a-section refusal.
#
# What: deletes the consumed fragments and verifies each one is gone. Any
# survivor is reported by name, together with the fact that CHANGELOG.md is
# ALREADY updated and that re-running would duplicate the bullets, and returns
# nonzero so the caller's success message never prints. Existence is the ground
# truth here, not `rm`'s exit status — one status covers N files.
# Test: the `undeletable-fragment-*` cases in scripts/assemble_changelog_selftest.sh.
delete_consumed_fragments() {
  local crate_dir="$1" what="$2"
  shift 2
  local fragments=("$@") survivors=() f

  rm -f "${fragments[@]}" || true
  for f in "${fragments[@]}"; do
    if [[ -e "${f}" ]]; then survivors+=("${f}"); fi
  done
  [[ "${#survivors[@]}" -eq 0 ]] && return 0

  echo "ERROR: crates/${crate_dir}/CHANGELOG.md WAS ALREADY UPDATED (${what}), but" >&2
  echo "       ${#survivors[@]} of ${#fragments[@]} fragment(s) could not be deleted:" >&2
  printf '%s\n' "${survivors[@]}" | sed 's#^.*/changelog\.d/#         changelog.d/#' >&2
  echo "       Do NOT re-run: those bullets are in CHANGELOG.md already, and a" >&2
  echo "       second run would add them a second time. Delete these by hand," >&2
  echo "       then review and commit the CHANGELOG.md change." >&2
  return 1
}

merge_fragments() {
  local crate_dir="$1" changelog="$2" version="$3" parsed="$4"
  shift 4
  local fragments=("$@")

  local plan="${changelog}.merge-plan.$$"
  local report="${changelog}.merge-report.$$"
  local tmp="${changelog}.merge.$$"
  # shellcheck disable=SC2064  # expand the paths now so the trap targets these files
  trap "rm -f '${plan}' '${report}' '${tmp}'" EXIT

  render_merge_plan "${parsed}" >"${plan}"
  if [[ ! -s "${plan}" ]]; then
    echo "ERROR: no category groups rendered — refusing to merge nothing." >&2
    exit 1
  fi

  merge_into_section "${changelog}" "${version}" "${plan}" "${tmp}" "${report}"

  local want_cats got_cats
  want_cats="$(printf '%s\n' "${parsed}" | cut -f1 | LC_ALL=C sort -u)"
  got_cats="$(cut -f2 "${report}" | LC_ALL=C sort -u)"
  if [[ "${want_cats}" != "${got_cats}" ]]; then
    echo "ERROR: merge into '## [${version}]' did not place every pending category." >&2
    echo "       Expected: $(printf '%s' "${want_cats}" | tr '\n' ' ')" >&2
    echo "       Placed:   $(printf '%s' "${got_cats}" | tr '\n' ' ')" >&2
    echo "       crates/${crate_dir}/CHANGELOG.md and changelog.d/ are UNCHANGED." >&2
    exit 1
  fi

  mv "${tmp}" "${changelog}"
  rm -f "${plan}" "${report}"
  trap - EXIT

  delete_consumed_fragments "${crate_dir}" "merged into [${version}]" "${fragments[@]}" || exit 1

  echo "Merged ${#fragments[@]} fragment(s) into the existing [${version}] section of" >&2
  echo "crates/${crate_dir}/CHANGELOG.md and removed them from changelog.d/." >&2
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
      write | --check | --merge) ;;
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

  local has_section=0
  if grep -qE "^## \[${version//./\\.}\]" "${changelog}"; then
    has_section=1
  fi

  # --merge folds into the existing section; every other mode refuses to touch
  # one. See the STRANDED FRAGMENTS note at the top of this file (#5298).
  if [[ "${mode}" == "--merge" ]]; then
    if [[ "${has_section}" -eq 0 ]]; then
      echo "ERROR: ${changelog} has no '## [${version}]' section to merge into." >&2
      echo "       --merge exists to recover a section that was assembled for a cut" >&2
      echo "       that never published; there is nothing here to recover. Drop" >&2
      echo "       --merge to assemble a fresh section. Nothing has been modified." >&2
      exit 1
    fi
    merge_fragments "${crate_dir}" "${changelog}" "${version}" "${parsed}" "${fragments[@]}"
    return 0
  fi

  if [[ "${has_section}" -eq 1 ]]; then
    echo "ERROR: ${changelog} already has a '## [${version}]' section, and these" >&2
    echo "       ${#fragments[@]} fragment(s) are still pending behind it:" >&2
    printf '%s\n' "${fragments[@]}" | sed "s#^${crate_path}/#         #" >&2
    echo "       Refusing to insert a second section. Nothing has been modified." >&2
    echo "" >&2
    echo "       This is the #5298 state: an earlier run assembled [${version}] for a" >&2
    echo "       cut that did not publish, and work accumulated behind it. Two ways" >&2
    echo "       forward — never hand-edit CHANGELOG.md:" >&2
    echo "         1. Fold the pending fragments into that section:" >&2
    echo "              scripts/assemble-changelog.sh ${crate_dir} ${version} --merge" >&2
    echo "         2. Cut the next version instead, leaving [${version}] as history." >&2
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
  delete_consumed_fragments "${crate_dir}" "assembled as [${version}]" "${fragments[@]}" || exit 1

  echo "Assembled ${#fragments[@]} fragment(s) into crates/${crate_dir}/CHANGELOG.md as [${version}]" >&2
  echo "and removed them from crates/${crate_dir}/changelog.d/." >&2
}

main "$@"
