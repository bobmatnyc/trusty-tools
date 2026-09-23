#!/usr/bin/env bash
#
# semver_accepted_breaks.sh — the owner-declared exception to preflight CHECK 5's
# BREAK verdict. Sourced by scripts/preflight-publish.sh.
#
# Why: CHECK 5 stops every publish whose public API breaks without a breaking
#   version bump, and until 2026-09-22 nothing could clear that. The owner then
#   ruled: "accept the breaking API changes on main and keep the existing release
#   plan. Override the semver gate (CHECK 5) so 1.x releases can publish with
#   them" — because "the user base is small, and it does not dictate the release
#   model." This file is that override. It is scoped to ONE crate at ONE version
#   and to the breaks it lists, so it cannot become a way to switch the gate off.
#
# What: a computed break for <package> <version> is ACCEPTED only when the
#   committed file scripts/semver-accepted-breaks/<package>-<version>.txt exists
#   and all of these hold; any other state is [FAIL]:
#     - exactly one `crate` row, equal to <package>;
#     - exactly one `version` row, equal to <version> AND to the version the
#       gate's `CHECK <package>: <base> -> <current>` line compared;
#     - exactly one `reason` row, not blank;
#     - at least one `accept <lint> <item>...` row, and no unknown row;
#     - the gate output parses: exactly one `CHECK <package>:` comparison, no
#       NO VERDICT line, and one `--- failure <lint>:` block per failed lint
#       counted in the tool's `N checks: P pass, F fail` line, each block with
#       at least one `Failed in:` entry;
#     - EVERY `Failed in:` entry of EVERY failed lint is covered by an accept row
#       for that lint whose item tokens all occur in the entry as whole tokens.
#   Row format and an example: scripts/semver-accepted-breaks/README.md.
#   A declaration never stands in for a verdict the gate did not produce: the
#   blind arm stays governed by PREFLIGHT_SEMVER_UNVERIFIED alone, and only
#   prints semver_accept_blind_note below.
#
# Test: scripts/preflight-check5-selftest.sh, the accepted-break cases (a)-(g).
#
# Portability: bash 3.2 and bash 5; BSD and GNU awk/sed. Reads REPO_ROOT from
#   the caller; sets SEMVER_ACCEPTED_BREAKS and SEMVER_GATE_COMPARED.

# Set when a declaration accepted a break; read by the final summary line.
SEMVER_ACCEPTED_BREAKS=""

# semver_accept_rel <package> <version> — the declaration's repo-relative path.
semver_accept_rel() {
  printf 'scripts/semver-accepted-breaks/%s-%s.txt' "$1" "$2"
}

# semver_accept_blind_note <package> <version> — on the blind arm, say that a
# declaration present for this release does not apply there. Always returns 0.
semver_accept_blind_note() {
  local rel
  rel="$(semver_accept_rel "$1" "$2")"
  [ -f "${REPO_ROOT}/${rel}" ] || return 0
  echo "       ${rel} exists, but a declaration accepts only breaks a completed" >&2
  echo "       comparison COMPUTED. It does not cover a gate that produced no verdict;" >&2
  echo "       that case is PREFLIGHT_SEMVER_UNVERIFIED's alone." >&2
  return 0
}

# semver_accept_parse <decl> <package> <version> <accept-out> <err-out> — read
# the declaration. Writes `<lint>\t<item tokens>` rows to <accept-out> and one
# problem per line to <err-out>; sets SEMVER_ACCEPT_REASON.
SEMVER_ACCEPT_REASON=""
semver_accept_parse() {
  local decl="$1" pkg="$2" version="$3" out="$4" err="$5"
  local line key rest lint items lineno=0
  local n_crate=0 n_version=0 n_reason=0 n_accept=0 d_crate="" d_version=""
  SEMVER_ACCEPT_REASON=""
  : > "$out"
  : > "$err"
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    line="${line%$'\r'}"
    key=""
    rest=""
    read -r key rest <<< "$line"
    case "$key" in
      ''|'#'*) continue ;;
      crate) n_crate=$((n_crate + 1)); d_crate="$rest" ;;
      version) n_version=$((n_version + 1)); d_version="$rest" ;;
      reason) n_reason=$((n_reason + 1)); SEMVER_ACCEPT_REASON="$rest" ;;
      accept)
        lint=""
        items=""
        read -r lint items <<< "$rest"
        if ! printf '%s\n' "$lint" | grep -Eq '^[a-z][a-z0-9_]*$' || [ -z "$items" ]; then
          echo "line ${lineno}: an accept row is 'accept <lint> <item>...', got: ${line}" >> "$err"
          continue
        fi
        n_accept=$((n_accept + 1))
        printf '%s\t%s\n' "$lint" "$(printf '%s' "$items" | tr -s '[:space:]' ' ')" >> "$out"
        ;;
      *) echo "line ${lineno}: unknown row '${key}' (rows are crate, version, reason, accept)" >> "$err" ;;
    esac
  done < "$decl"

  if [ "$n_crate" -ne 1 ]; then
    echo "needs exactly one 'crate' row, found ${n_crate}" >> "$err"
  elif [ "$d_crate" != "$pkg" ]; then
    echo "names crate '${d_crate}', but this publish is '${pkg}'" >> "$err"
  fi
  if [ "$n_version" -ne 1 ]; then
    echo "needs exactly one 'version' row, found ${n_version}" >> "$err"
  elif [ "$d_version" != "$version" ]; then
    echo "names version '${d_version}', but this publish is '${version}'" >> "$err"
  fi
  if [ "$n_reason" -ne 1 ]; then
    echo "needs exactly one 'reason' row, found ${n_reason} — the reason is the record of why the break was accepted" >> "$err"
  elif [ -z "$(printf '%s' "$SEMVER_ACCEPT_REASON" | tr -d '[:space:]')" ]; then
    echo "the 'reason' row is empty — the reason is the record of why the break was accepted" >> "$err"
  fi
  if [ "$n_accept" -lt 1 ]; then
    echo "lists no 'accept <lint> <item>...' row" >> "$err"
  fi
  return 0
}

# semver_break_entries <gate-log> <package> — print one `<lint>\t<entry>` line per
# `Failed in:` entry, location stripped. On any inconsistency print one
# `ERROR\t<why>` line instead and return 1: a list this cannot read completely
# is never compared against a declaration.
semver_break_entries() {
  local log="$1" pkg="$2" esc clean n_check
  esc="$(printf '\033')"
  clean="$(sed "s/${esc}\[[0-9;]*m//g" "$log")"

  n_check="$(printf '%s\n' "$clean" | grep -c "^CHECK " || true)"
  if [ "$n_check" -ne 1 ] || ! printf '%s\n' "$clean" | grep -q "^CHECK ${pkg}: "; then
    printf 'ERROR\texpected exactly one "CHECK %s:" comparison, found %s\n' "$pkg" "$n_check"
    return 1
  fi
  if printf '%s\n' "$clean" | grep -Eq '^NO (VERDICT|INVENTORY) '; then
    printf 'ERROR\tthe gate also reported NO VERDICT, so part of the API was never compared\n'
    return 1
  fi

  printf '%s\n' "$clean" | awk '
    function close_block() { if (inblock && nent == 0) empty = empty " " lint; inblock = 0 }
    /^--- failure [A-Za-z0-9_]+: / {
      close_block(); lint = $3; sub(/:$/, "", lint); blocks++; inblock = 1; nent = 0; state = 1; next
    }
    /^--- / { close_block(); state = 0; next }
    / checks: [0-9]+ pass, [0-9]+ fail/ {
      for (i = 2; i <= NF; i++) if ($i == "fail,") fails += $(i - 1)
      counted++; next
    }
    state == 1 && /^Failed in:/ { state = 2; next }
    state == 2 && /^  [^ ]/ {
      e = substr($0, 3); p = index(e, " in /"); if (p > 0) e = substr(e, 1, p - 1)
      sub(/,$/, "", e); print lint "\t" e; nent++; next
    }
    state == 2 { state = 0 }
    END {
      close_block()
      if (counted != 1) { print "ERROR\tfound " counted + 0 " cargo-semver-checks \"N checks: P pass, F fail\" lines, expected 1"; exit 1 }
      if (blocks == 0) { print "ERROR\tno \"--- failure <lint>:\" block in the gate output"; exit 1 }
      if (blocks != fails) { print "ERROR\tcargo-semver-checks counted " fails + 0 " failed lint(s) but " blocks " failure block(s) parsed"; exit 1 }
      if (empty != "") { print "ERROR\tno \"Failed in:\" entry parsed for:" empty; exit 1 }
    }'
}

# semver_accept_match <accept-rows> <break-entries> — print COVERED/UNCOVERED per
# entry, then UNUSED per accept row that covered nothing. An entry is covered by a
# row for the same lint whose every item token occurs in it as a whole token —
# bounded by anything but [A-Za-z0-9_] — so `ManagedError` never covers
# `ResumeManagedError`.
semver_accept_match() {
  awk -F '\t' '
    function istok(c) { return c ~ /[A-Za-z0-9_]/ }
    function has_token(s, t,   p, off, b, a) {
      off = 0
      while ((p = index(substr(s, off + 1), t)) > 0) {
        p += off
        b = (p > 1) ? substr(s, p - 1, 1) : ""
        a = substr(s, p + length(t), 1)
        if ((b == "" || !istok(b)) && (a == "" || !istok(a))) return 1
        off = p
      }
      return 0
    }
    FNR == NR { n++; alint[n] = $1; aitems[n] = $2; next }
    {
      hit = 0
      for (i = 1; i <= n; i++) {
        if (alint[i] != $1) continue
        k = split(aitems[i], toks, " "); ok = 1
        for (j = 1; j <= k; j++) if (!has_token($2, toks[j])) { ok = 0; break }
        if (ok) { hit = 1; used[i] = 1 }
      }
      print (hit ? "COVERED" : "UNCOVERED") "\t" $1 "\t" $2
    }
    END { for (i = 1; i <= n; i++) if (!used[i]) print "UNUSED\t" alint[i] "\t" aitems[i] }
  ' "$1" "$2"
}

# semver_accept_provenance <rel> — say whether the declaration is committed, and
# by whom. Informational: CHECK 1 and CHECK 3 are what force it onto main.
semver_accept_provenance() {
  local rel="$1" who
  if git -C "$REPO_ROOT" ls-files --error-unmatch -- "$rel" > /dev/null 2>&1 \
    && git -C "$REPO_ROOT" diff --quiet HEAD -- "$rel" 2> /dev/null; then
    who="$(git -C "$REPO_ROOT" log -1 --format='%h by %an on %ad' --date=short -- "$rel" 2> /dev/null)"
    echo "committed (${who})"
  else
    echo "NOT COMMITTED — CHECK 3 refuses a publish carrying it; land it on main in a reviewed PR"
  fi
}

# semver_accept_decide <gate-log> <package> <version> — decide a computed BREAK
# when a declaration file exists for this exact release. Returns 0 to permit
# (prints [WARN]), 1 to stop (prints [FAIL]).
semver_accept_decide() {
  local log="$1" pkg="$2" version="$3"
  local rel decl work lints n_items lint items tab compared_to rc=0
  tab="$(printf '\t')"
  rel="$(semver_accept_rel "$pkg" "$version")"
  decl="${REPO_ROOT}/${rel}"
  work="$(mktemp -d "${TMPDIR:-/tmp}/preflight-accept.XXXXXX")"

  semver_accept_parse "$decl" "$pkg" "$version" "${work}/accept" "${work}/err"
  semver_break_entries "$log" "$pkg" > "${work}/entries" || true

  # The declaration binds to the version the gate COMPARED (the manifest's), not
  # only to the version argument; an unreadable CHECK line is refused too.
  compared_to="$(sed -n "s/^CHECK ${pkg}: [^ ]* -> \([^ ]*\) .*/\1/p" "$log" | head -1)"
  if [ "$compared_to" != "$version" ]; then
    echo "names version '${version}', but the gate compared ${pkg} '${compared_to:-<unknown>}' (the manifest version) — the breaks it lists belong to that release" >> "${work}/err"
  fi

  if [ -s "${work}/err" ]; then
    echo "[FAIL] semver: ${pkg} ${version} breaks its public API, and ${rel} is not" >&2
    echo "       a valid declaration, so nothing is accepted:" >&2
    sed 's/^/         /' "${work}/err" >&2
    rc=1
  elif grep -q '^ERROR' "${work}/entries"; then
    echo "[FAIL] semver: ${pkg} ${version} breaks its public API, and its break list" >&2
    echo "       could not be read completely out of the gate output, so" >&2
    echo "       ${rel} cannot be checked against it:" >&2
    grep '^ERROR' "${work}/entries" | cut -f2- | sed 's/^/         /' >&2
    rc=1
  else
    LC_ALL=C sort -u "${work}/entries" > "${work}/entries.u"
    semver_accept_match "${work}/accept" "${work}/entries.u" > "${work}/match"
    if grep -q '^UNCOVERED' "${work}/match"; then
      echo "[FAIL] semver: ${pkg} ${version} has breaks that ${rel} does not declare:" >&2
      grep '^UNCOVERED' "${work}/match" | cut -f2- | sed "s/${tab}/: /; s/^/         NOT DECLARED  /" >&2
      echo "       A declaration accepts the breaks it lists and nothing else. Add an" >&2
      echo "       'accept <lint> <item>' row for each one only if the owner accepts it too." >&2
      rc=1
    fi
  fi

  if [ "$rc" -ne 0 ]; then
    echo "       Full gate output:" >&2
    sed 's/^/       /' "$log" >&2
    rm -rf "$work"
    return 1
  fi

  lints="$(cut -f1 "${work}/entries.u" | LC_ALL=C sort -u | tr '\n' ' ' | sed 's/ $//; s/ /, /g')"
  n_items="$(wc -l < "${work}/entries.u" | tr -d ' ')"
  # shellcheck disable=SC2034  # read by preflight-publish.sh's final summary
  SEMVER_ACCEPTED_BREAKS="$(cut -f1 "${work}/entries.u" | LC_ALL=C sort -u | wc -l | tr -d ' ') breaking lint(s) accepted by ${rel}"
  # The gate did compare this crate and built fresh rustdoc, so the type differ
  # may read that cache (see semver_types_advisory).
  # shellcheck disable=SC2034  # read by check5_semver in preflight-publish.sh
  SEMVER_GATE_COMPARED=1
  echo "[WARN] semver: ACCEPTED BREAK — ${pkg} ${version} ships a public-API break its" >&2
  echo "       version bump does not carry, accepted by ${rel}." >&2
  echo "       Accepted lints: ${lints}" >&2
  echo "       Reason: ${SEMVER_ACCEPT_REASON}" >&2
  echo "       Declaration: $(semver_accept_provenance "$rel")" >&2
  echo "       Gate compared: $(grep '^CHECK ' "$log" | head -1)" >&2
  echo "       Accept rows:" >&2
  while IFS="$tab" read -r lint items; do
    if grep -Fxq "UNUSED${tab}${lint}${tab}${items}" "${work}/match"; then
      echo "         accept ${lint} ${items}   (covered nothing this run)" >&2
    else
      echo "         accept ${lint} ${items}" >&2
    fi
  done < "${work}/accept"
  echo "       Computed break list, ${n_items} item(s), every one declared:" >&2
  sed "s/${tab}/: /; s/^/         /" "${work}/entries.u" >&2
  echo "       This is NOT a pass. A dependent whose requirement admits ${version} can" >&2
  echo "       stop compiling on it." >&2
  echo "       The declaration covers ${pkg} ${version} only; see docs/reference/semver-gate.md," >&2
  echo "       \"Accepted breaks\"." >&2
  rm -rf "$work"
  return 0
}
