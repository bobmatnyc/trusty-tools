#!/usr/bin/env bash
#
# check_doc_numbers.sh — document-number allocation lint (DOC-N / ADR-NNNN).
#
# Why: nothing owned `DOC-N` / ADR-number allocation, so two authors working
#   concurrently routinely claimed the same number and nobody noticed until a
#   human happened to read both documents. Four collisions surfaced in a single
#   session:
#     - DOC-42: docs/specs/agent-bundled-skills.md self-labels DOC-42, while
#       ADR-0016 textually reserves DOC-42 for the Engineering Lead / Virtual
#       Twin work (PR #3006, closed unmerged; six phase issues #3007-#3012
#       reference it). That work now appears to claim DOC-44 instead.
#     - DOC-46: issue #3169 reserved DOC-46 for a control-bus spec;
#       docs/specs/DOC-46-adr-standard.md (PR #3172) took the slot first.
#     - ADR-0023: two branches independently wrote an ADR-0023; one merged
#       (#4237 worktree-authority) and the other was renumbered to 0024 at
#       commit time — caught by luck, not by a gate.
#     - A stale "next free DOC-N" hint in docs/specs/README.md said DOC-60 when
#       60 was already taken, and actively misled a later author.
#   Advice without a gate loses (the same premise as scripts/check_line_cap.sh).
#   This script turns number allocation into a mechanical CI gate whose
#   grandfather allowlist can only SHRINK.
#
# What: four checks over tracked Markdown.
#
#   1. DUP-DOC / DUP-ADR — no two FILES may claim the same number. A file
#      "claims" a number via its filename (`DOC-46-adr-standard.md`,
#      `0021-cargo-bin-policy.md`) and/or its own header self-label (`# DOC-42 —
#      …`, a `**DOC-28**` status badge, `# 0021. …`). One file claiming the same
#      number both ways is agreement, not a collision; two DIFFERENT files
#      claiming one number is a collision, reported with every colliding path.
#
#   2. LABEL-MISMATCH — a file whose filename number and whose own header
#      self-label disagree (filename says DOC-61, header says DOC-59). That is a
#      defect in its own right and a reliable precursor to a collision.
#
#   3. CATALOG-DANGLING — every `| DOC-N | … |` row in the spec catalog
#      (docs/specs/README.md) must point at a file that exists.
#      DELIBERATELY ONE-DIRECTIONAL: the OTHER direction (every spec doc must
#      have a catalog row, DOC-38 §4.5) is ALREADY enforced by the `spec-catalog`
#      check in crates/trusty-sld-lint (see checks.rs::check_catalog_row, run via
#      scripts/check_sld.sh). This script does not duplicate it — it adds only
#      the half that had no owner: a catalog row pointing at a file that was
#      renamed or never landed.
#
#   4. NEXT-FREE — if docs/specs/README.md carries a "Next free `DOC-N` = …"
#      hint, the hinted number must actually be unclaimed (no file claims it, no
#      catalog row uses it). A stale hint is worse than no hint: it reads as
#      authority and caused a real collision.
#
# EXPLICIT NON-GOAL — prose reservations. ADR-0016 reserves DOC-42 in ordinary
#   body text; issue #3169 reserved DOC-46 in a GitHub issue. Neither is
#   mechanically detectable without heuristics that would fire on every
#   incidental "see DOC-42" cross-reference in the corpus — and this repo's
#   documents are dense with those. Mining prose for reservations is therefore
#   OUT OF SCOPE BY DESIGN, not an oversight: the boundary is "a number a
#   document claims for ITSELF (filename or own header)" versus "a number a
#   document merely mentions". Only the former is linted. A consequence worth
#   stating plainly: this gate does NOT and CANNOT catch the DOC-42 collision
#   that motivated it — that one needs a human ledger (docs/specs/README.md's
#   collision note) or an issue-tracker convention. What it DOES catch is the
#   mechanical majority: two files, one number.
#
# NAMESPACES (scoped deliberately; each is an independent number line):
#   DOC — docs/specs/**  plus  docs/trusty-installer/research/02-design/**.
#         Those are ONE number line historically: the installer research charter
#         claimed DOC-0…DOC-12 and docs/specs continues at DOC-13. Both are
#         scanned for duplicates so a new docs/specs/DOC-5-*.md is caught.
#         Only docs/specs/** participates in the CATALOG checks — the installer
#         research set is a frozen historical charter with no catalog.
#   ADR — docs/adr/** only. The per-crate `docs/*/decisions/**` directories
#         (trusty-agents, trusty-memory, …) each run their own independent
#         0001.. sequence; they are NOT the workspace ADR line and a `0001` in
#         two different crates is correct, not a collision.
#
# ALLOWLIST (ratchet, can only shrink — mirrors .line-cap-allowlist.tsv and
#   .test-pointer-allowlist.tsv): `.doc-number-allowlist.tsv`, one
#   `<CODE><TAB><key>` row per grandfathered violation. A listed violation that
#   still holds is suppressed (OK); a listed violation that no longer holds is a
#   FAIL — remove its entry (`--update` does this automatically). Ordinary
#   `--update` never ADDS a row; `--seed` is the one-time bootstrap that
#   grandfathers every current violation (refuses to run if the allowlist
#   already has rows). Pre-existing collisions are grandfathered rather than
#   renumbered because renumbering a live DOC-N rewrites every inbound
#   reference — an owner decision, not a lint's call.
#
# Usage:
#   check_doc_numbers.sh              # check mode (default); exit 0 = clean
#   check_doc_numbers.sh --update      # prune allowlist rows that no longer hold
#   check_doc_numbers.sh --seed        # bootstrap: grandfather current violations
#   check_doc_numbers.sh --self-test   # fixture-based self-test (see run_self_test)
#
# Test: `--self-test` builds a throwaway repo exercising each violation class
#   (duplicate by filename, duplicate by self-label, label mismatch, dangling
#   catalog row, stale next-free hint) plus the clean cases that must NOT fire
#   (one file claiming a number twice consistently; a prose mention of another
#   document's number). Wired into .github/workflows/doc-numbers.yml ahead of
#   the real gate, matching line-cap.yml's selftest-then-gate ordering.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); POSIX tools + git only.
#   No associative arrays.

set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"

# Header region scanned for a self-label. Every real header block in this repo
# puts its DOC-N/ADR label in the H1 or the status-badge line immediately under
# it (optionally after a short `---` frontmatter block). Bounding the scan is
# what keeps ordinary body prose — "…DOC-32 supersedes it" — from reading as a
# self-claim.
HEADER_LINES=10

# ---------------------------------------------------------------------------
# first_number: awk helper printing the first digit run in a string as an
# integer (so `0021` -> 21, `DOC-0` -> 0). Shared by every label parser.
# ---------------------------------------------------------------------------
NUM_AWK='
function first_number(s) {
  if (match(s, /[0-9]+/)) return substr(s, RSTART, RLENGTH) + 0
  return -1
}
'

# ---------------------------------------------------------------------------
# doc_label_of: print the DOC number a file self-labels in its header region,
# or nothing. Recognised forms (both live in this repo today):
#   `# DOC-42 — Agent-Bundled Skills…`        (H1 title, the dominant form)
#   `**DOC-28** | Status: `Draft` | Date: …`  (status-badge line, older specs)
# ---------------------------------------------------------------------------
doc_label_of() {
  awk -v maxl="$HEADER_LINES" "$NUM_AWK"'
    NR > maxl { exit }
    /^# DOC-[0-9]+/       { print first_number(substr($0, 3)); exit }
    /^\*\*DOC-[0-9]+\*\*/ { print first_number(substr($0, 3)); exit }
  ' "$1"
}

# ---------------------------------------------------------------------------
# adr_label_of: print the ADR number a file self-labels in its header region.
# The repo's ADR H1 form is `# 0021. Title` (see docs/adr/template.md).
# ---------------------------------------------------------------------------
adr_label_of() {
  awk -v maxl="$HEADER_LINES" "$NUM_AWK"'
    NR > maxl { exit }
    /^# 0*[0-9]+\./ { print first_number($0); exit }
  ' "$1"
}

# ---------------------------------------------------------------------------
# collect_claims: write "<NS>\t<num>\t<path>\t<via>" rows to file $1 — one per
# (file, number) claim. <via> is `filename`, `header`, or `both`, used only to
# make the FAIL message actionable.
#
# A file that claims one number by filename AND the same number by header emits
# a single `both` row: self-agreement is not a collision. A file that claims two
# DIFFERENT numbers emits both rows AND a LABEL-MISMATCH (raised by the caller,
# which is where the mismatch is detectable from the pair).
# ---------------------------------------------------------------------------
collect_claims() {
  local out="$1"
  : > "$out"

  # #4618: both enumerations are materialised first so a failing `git ls-files`
  # is observable. Piping it straight into `while` hid a git error behind an
  # empty stream, and an empty claim set produces "0 violations — OK".
  local doclist adrlist
  doclist="$(mktemp "${TMPDIR:-/tmp}/docnum.doclist.XXXXXX")"
  adrlist="$(mktemp "${TMPDIR:-/tmp}/docnum.adrlist.XXXXXX")"
  if ! git ls-files 'docs/specs/*.md' 'docs/trusty-installer/research/02-design/*.md' > "$doclist" ||
     ! git ls-files 'docs/adr/*.md' > "$adrlist"; then
    echo "FAIL: TOOL ERROR — 'git ls-files' exited non-zero; the doc set could not" >&2
    echo "      be enumerated, so no number claims were collected. NOT a pass (#4618)." >&2
    rm -f "$doclist" "$adrlist"
    exit 1
  fi

  DOCS_SCANNED=0

  # ---- DOC namespace -------------------------------------------------------
  while IFS= read -r f; do
      [ -f "$f" ] || continue
      local base fnum lnum
      base="${f##*/}"
      fnum=""
      case "$base" in
        DOC-[0-9]*)
          fnum="$(printf '%s' "$base" | sed -nE 's/^DOC-0*([0-9]+)([-.].*)?$/\1/p')"
          ;;
      esac
      lnum="$(doc_label_of "$f")"
      DOCS_SCANNED=$((DOCS_SCANNED + 1))
      emit_claims DOC "$f" "$fnum" "$lnum" >> "$out"
    done < "$doclist"

  # ---- ADR namespace -------------------------------------------------------
  # Only `NNNN-slug.md` files; README.md / INDEX.md / template.md carry no
  # number of their own and must not be scanned as claimants.
  while IFS= read -r f; do
      [ -f "$f" ] || continue
      local base fnum lnum
      base="${f##*/}"
      case "$base" in
        [0-9][0-9][0-9][0-9]-*) ;;
        *) continue ;;
      esac
      fnum="$(printf '%s' "$base" | sed -nE 's/^0*([0-9]+)-.*/\1/p')"
      lnum="$(adr_label_of "$f")"
      DOCS_SCANNED=$((DOCS_SCANNED + 1))
      emit_claims ADR "$f" "$fnum" "$lnum" >> "$out"
    done < "$adrlist"

  rm -f "$doclist" "$adrlist"
}

# ---------------------------------------------------------------------------
# emit_claims: given namespace $1, path $2, filename-number $3 (may be empty),
# header-number $4 (may be empty), print the claim rows for that file.
# ---------------------------------------------------------------------------
emit_claims() {
  local ns="$1" path="$2" fnum="$3" lnum="$4"
  if [ -n "$fnum" ] && [ -n "$lnum" ]; then
    if [ "$fnum" = "$lnum" ]; then
      printf '%s\t%s\t%s\tboth\n' "$ns" "$fnum" "$path"
    else
      printf '%s\t%s\t%s\tfilename\n' "$ns" "$fnum" "$path"
      printf '%s\t%s\t%s\theader\n' "$ns" "$lnum" "$path"
    fi
  elif [ -n "$fnum" ]; then
    printf '%s\t%s\t%s\tfilename\n' "$ns" "$fnum" "$path"
  elif [ -n "$lnum" ]; then
    printf '%s\t%s\t%s\theader\n' "$ns" "$lnum" "$path"
  fi
}

# ---------------------------------------------------------------------------
# catalog_rows: print "<num>\t<relative-path>" for every spec-catalog table row
# in docs/specs/README.md ($1 = repo root). A row looks like:
#   | DOC-46 | `SPEC-ADR-01~draft` | [Title](./DOC-46-adr-standard.md) | … |
# Rows without a `](./path)` link are skipped (a catalog row with no link is a
# formatting problem DOC-38 owns, not a number-allocation one).
# ---------------------------------------------------------------------------
catalog_rows() {
  local readme="$1"
  [ -f "$readme" ] || return 0
  awk "$NUM_AWK"'
    /^\| *DOC-[0-9]+ *\|/ {
      n = first_number($0)
      # Link target: the first `](./…)` on the row.
      if (match($0, /\]\(\.\/[^)]+\)/)) {
        p = substr($0, RSTART + 4, RLENGTH - 5)
        print n "\t" p
      }
    }
  ' "$readme"
}

# ---------------------------------------------------------------------------
# next_free_hint: print the DOC number asserted free by README's "Next free"
# note, or nothing when the note is absent. The note's literal shape today is:
#   > **Next free `DOC-N` = `DOC-62`** (updated 2026-07-28 — …)
# `DOC-N` before the `=` is a placeholder, so the number is read from AFTER the
# first `=` on that line.
# ---------------------------------------------------------------------------
next_free_hint() {
  local readme="$1"
  [ -f "$readme" ] || return 0
  awk '
    /Next free/ {
      i = index($0, "=")
      if (i == 0) next
      rest = substr($0, i)
      if (match(rest, /DOC-[0-9]+/)) {
        print substr(rest, RSTART + 4, RLENGTH - 4) + 0
        exit
      }
    }
  ' "$readme"
}

# ---------------------------------------------------------------------------
# scan: run the full lint over the git repo rooted at cwd. Writes one
# "<CODE>\t<key>\t<message>" row per RAW violation (allowlist not yet applied)
# to the file named by $1. The caller applies the allowlist.
# ---------------------------------------------------------------------------
scan() {
  local raw_out="$1"
  : > "$raw_out"

  local claims readme
  claims="$(mktemp "${TMPDIR:-/tmp}/docnum.claims.XXXXXX")"
  readme="docs/specs/README.md"
  collect_claims "$claims"
  # #4618: number of (namespace, number, path) claims this run actually
  # collected — the count that distinguishes "checked every doc, found no
  # collision" from "checked nothing".
  CLAIMS_SCANNED="$(awk 'END{print NR}' "$claims")"

  # ---- 1. duplicate numbers (per namespace) --------------------------------
  # Group by (ns, num); a group covering more than one DISTINCT path collides.
  sort -t'	' -k1,1 -k2,2n -k3,3 "$claims" | awk -F'\t' '
    { key = $1 "\t" $2
      if (key != prev) {
        if (prev != "" && npaths > 1) emit()
        prev = key; delete paths; npaths = 0
      }
      if (!($3 in paths)) { paths[$3] = $4; order[++npaths] = $3 }
    }
    END { if (prev != "" && npaths > 1) emit() }
    function emit(   i, ns, num, list) {
      split(prev, kp, "\t"); ns = kp[1]; num = kp[2]
      list = ""
      for (i = 1; i <= npaths; i++) {
        list = list (i > 1 ? ", " : "") order[i] " (claims via " paths[order[i]] ")"
      }
      printf "DUP-%s\t%s\t%s-%s is claimed by %d files: %s\n", ns, num, ns, num, npaths, list
    }
  ' >> "$raw_out"

  # ---- 2. filename vs own-header mismatch ----------------------------------
  # A file appears twice in $claims with different numbers exactly when its
  # filename and header disagree (emit_claims collapses agreement to one row).
  sort -t'	' -k3,3 -k1,1 "$claims" | awk -F'\t' '
    { if ($3 == prevpath && $1 == prevns && $2 != prevnum) {
        printf "LABEL-MISMATCH\t%s\t%s filename claims %s-%s but its own header self-labels %s-%s\n", \
          $3, $3, $1, (prevvia == "filename" ? prevnum : $2), $1, (prevvia == "filename" ? $2 : prevnum)
      }
      prevpath = $3; prevns = $1; prevnum = $2; prevvia = $4
    }
  ' >> "$raw_out"

  # ---- 3. catalog row -> existing file (the half sld-lint does NOT cover) ---
  catalog_rows "$readme" | while IFS=$'\t' read -r n p; do
    [ -n "${n:-}" ] || continue
    if [ ! -f "docs/specs/$p" ]; then
      printf 'CATALOG-DANGLING\t%s\tcatalog row DOC-%s links to ./%s, which does not exist under docs/specs/\n' \
        "$n" "$n" "$p" >> "$raw_out"
    fi
  done

  # ---- 4. "next free DOC-N" hint accuracy ----------------------------------
  local hint
  hint="$(next_free_hint "$readme")"
  if [ -n "$hint" ]; then
    local taken_by
    taken_by="$(awk -F'\t' -v n="$hint" '$1 == "DOC" && $2 == n {print $3}' "$claims" | sort -u | tr '\n' ' ')"
    if [ -z "$taken_by" ]; then
      taken_by="$(catalog_rows "$readme" | awk -F'\t' -v n="$hint" '$1 == n {print "catalog row -> ./" $2}' | tr '\n' ' ')"
    fi
    if [ -n "$taken_by" ]; then
      printf 'NEXT-FREE\t%s\tdocs/specs/README.md advertises DOC-%s as the next free number, but it is already claimed by: %s\n' \
        "$hint" "$hint" "${taken_by% }" >> "$raw_out"
    fi
  fi

  rm -f "$claims"
}

# ---------------------------------------------------------------------------
# apply_allowlist: split raw violations $1 against allowlist $2 into surviving
# violations $3 and stale allowlist rows $4 (listed but no longer violating).
# Allowlist key is "<CODE>\t<key>" — the message column is never matched, so
# reworded messages never spuriously un-suppress a grandfathered row.
# ---------------------------------------------------------------------------
apply_allowlist() {
  local raw="$1" allowlist="$2" viol_out="$3" stale_out="$4"
  : > "$viol_out"
  : > "$stale_out"

  if [ ! -f "$allowlist" ]; then
    cp "$raw" "$viol_out"
    return 0
  fi

  local keys raw_keys
  keys="$(mktemp "${TMPDIR:-/tmp}/docnum.keys.XXXXXX")"
  raw_keys="$(mktemp "${TMPDIR:-/tmp}/docnum.rawkeys.XXXXXX")"
  awk -F'\t' '$0 !~ /^#/ && NF>=2 {print $1"\t"$2}' "$allowlist" | sort -u > "$keys"
  awk -F'\t' 'NF>=2 {print $1"\t"$2}' "$raw" | sort -u > "$raw_keys"

  while IFS=$'\t' read -r code key msg; do
    [ -n "${code:-}" ] || continue
    if grep -F -q -x -- "$(printf '%s\t%s' "$code" "$key")" "$keys"; then
      : # grandfathered — suppress
    else
      printf '%s\t%s\t%s\n' "$code" "$key" "$msg" >> "$viol_out"
    fi
  done < "$raw"

  while IFS=$'\t' read -r code key; do
    [ -n "${code:-}" ] || continue
    if ! grep -F -q -x -- "$(printf '%s\t%s' "$code" "$key")" "$raw_keys"; then
      printf '%s\t%s\n' "$code" "$key" >> "$stale_out"
    fi
  done < "$keys"

  rm -f "$keys" "$raw_keys"
}

# ---------------------------------------------------------------------------
# prune_stale_from_allowlist: rewrite allowlist $1 in place, dropping every
# (code, key) row present in stale file $2. Factored out so the self-test can
# exercise the exact `--update` code path (mirrors check_test_pointers.sh).
# ---------------------------------------------------------------------------
prune_stale_from_allowlist() {
  local allowlist="$1" stale_file="$2" new
  new="$(mktemp "${TMPDIR:-/tmp}/docnum.new.XXXXXX")"
  awk -F'\t' -v stalefile="$stale_file" '
    BEGIN { while ((getline line < stalefile) > 0) stale[line] = 1 }
    /^#/ { print; next }
    NF < 2 { print; next }
    { key = $1"\t"$2; if (!(key in stale)) print }
  ' "$allowlist" > "$new"
  mv "$new" "$allowlist"
}

# ---------------------------------------------------------------------------
# run_self_test: throwaway git repo exercising every violation class plus the
# clean cases that must NOT fire.
# ---------------------------------------------------------------------------
run_self_test() {
  local tmp ok=1
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/docnum-selftest.XXXXXX")"
  trap 'rm -rf "$tmp"' RETURN

  mkdir -p "$tmp/docs/specs" "$tmp/docs/adr"

  # Clean: filename and header agree, cataloged. Must never fire.
  cat > "$tmp/docs/specs/DOC-10-clean.md" <<'EOF'
# DOC-10 — Clean Spec

Body prose mentioning DOC-11 and DOC-99 as ordinary cross-references, which
must NOT read as a self-claim (prose mining is out of scope by design).
EOF

  # DUP-DOC by two headers (badge form vs H1 form), neither DOC-prefixed name.
  cat > "$tmp/docs/specs/alpha.md" <<'EOF'
# DOC-20 — Alpha
EOF
  cat > "$tmp/docs/specs/beta.md" <<'EOF'
Title line

**DOC-20** | Status: `Draft` | Date: 2026-07-28
EOF

  # LABEL-MISMATCH: filename says 30, header says 31.
  cat > "$tmp/docs/specs/DOC-30-mismatch.md" <<'EOF'
# DOC-31 — Mismatched Self-Label
EOF

  # DUP-ADR by filename+header on two files.
  cat > "$tmp/docs/adr/0007-first.md" <<'EOF'
# 0007. First decision
EOF
  cat > "$tmp/docs/adr/0007-second.md" <<'EOF'
# 0007. Second decision
EOF
  # Clean ADR plus non-claimant files that must be skipped entirely.
  cat > "$tmp/docs/adr/0008-fine.md" <<'EOF'
# 0008. Fine decision
EOF
  cat > "$tmp/docs/adr/README.md" <<'EOF'
# 0001. This looks like an ADR but README.md is not a numbered claimant
EOF

  # Catalog: one good row, one dangling row, and a stale next-free hint
  # (DOC-20 is claimed by alpha.md/beta.md, so advertising it is a defect).
  cat > "$tmp/docs/specs/README.md" <<'EOF'
# Specs

| DOC | Spec ID | Title | Subsystem |
|-----|---------|-------|-----------|
| DOC-10 | `SPEC-A-01~draft` | [Clean Spec](./DOC-10-clean.md) | x |
| DOC-12 | `SPEC-B-01~draft` | [Vanished Spec](./DOC-12-vanished.md) | x |

> **Next free `DOC-N` = `DOC-20`** (updated 2026-07-28).
EOF

  ( cd "$tmp" && git init -q && git add -A && git -c user.email=t@t -c user.name=t commit -q -m fixture )

  local raw
  raw="$(mktemp "${TMPDIR:-/tmp}/docnum.self.raw.XXXXXX")"
  ( cd "$tmp" && scan "$raw" )

  local want
  for want in \
    'DUP-DOC	20' \
    'DUP-ADR	7' \
    'LABEL-MISMATCH	docs/specs/DOC-30-mismatch.md' \
    'CATALOG-DANGLING	12' \
    'NEXT-FREE	20'
  do
    if ! grep -qF "$want" "$raw"; then
      echo "self-test FAIL: expected a violation keyed '$want', got:" >&2
      cat "$raw" >&2
      ok=0
    fi
  done

  # Exactly five: nothing else may fire. In particular DOC-10 (consistent
  # filename+header, cataloged), ADR-0008, docs/adr/README.md's decoy heading,
  # and DOC-10-clean.md's prose mentions of DOC-11/DOC-99 must all stay silent.
  if [ "$(wc -l < "$raw" | tr -d ' ')" != "5" ]; then
    echo "self-test FAIL: expected exactly 5 violations, got:" >&2
    cat "$raw" >&2
    ok=0
  fi

  # ---- allowlist ratchet: suppress one real violation, prune one stale row --
  local allow viol stale
  allow="$tmp/.doc-number-allowlist.tsv"
  viol="$(mktemp "${TMPDIR:-/tmp}/docnum.self.viol.XXXXXX")"
  stale="$(mktemp "${TMPDIR:-/tmp}/docnum.self.stale.XXXXXX")"
  {
    printf 'DUP-DOC\t20\n'
    printf 'DUP-ADR\t999\n'
  } > "$allow"
  apply_allowlist "$raw" "$allow" "$viol" "$stale"

  if grep -qF 'DUP-DOC	20' "$viol"; then
    echo "self-test FAIL: DUP-DOC 20 is allowlisted but was still reported:" >&2
    cat "$viol" >&2
    ok=0
  elif ! grep -qF 'DUP-ADR	7' "$viol"; then
    echo "self-test FAIL: DUP-ADR 7 is NOT allowlisted but was suppressed (allowlist not scoped to (code,key)):" >&2
    cat "$viol" >&2
    ok=0
  elif [ "$(wc -l < "$stale" | tr -d ' ')" != "1" ] || ! grep -qF 'DUP-ADR	999' "$stale"; then
    echo "self-test FAIL: expected exactly 1 stale allowlist row (DUP-ADR 999), got:" >&2
    cat "$stale" >&2
    ok=0
  fi

  if [ "$ok" -eq 1 ]; then
    prune_stale_from_allowlist "$allow" "$stale"
    if grep -qF 'DUP-ADR	999' "$allow"; then
      echo "self-test FAIL: --update did not prune the stale DUP-ADR 999 row:" >&2
      cat "$allow" >&2
      ok=0
    elif ! grep -qF 'DUP-DOC	20' "$allow"; then
      echo "self-test FAIL: --update incorrectly pruned the still-valid DUP-DOC 20 row:" >&2
      cat "$allow" >&2
      ok=0
    fi
  fi

  rm -f "$raw" "$viol" "$stale"

  if [ "$ok" -eq 1 ]; then
    echo "check_doc_numbers self-test: OK (duplicate DOC/ADR numbers caught via filename and self-label, label mismatch caught, dangling catalog row caught, stale next-free hint caught, consistent/cataloged docs and prose cross-references stay silent, allowlist ratchet suppresses and prunes correctly by (code,key))."
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
    --update)    MODE="update" ;;
    --seed)      MODE="seed" ;;
    --self-test) MODE="self-test" ;;
    -h|--help)
      grep '^#' "$SCRIPT_PATH" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check_doc_numbers: unknown argument: $arg" >&2
      echo "usage: check_doc_numbers.sh [--update | --seed | --self-test]" >&2
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
ALLOWLIST="$REPO_ROOT/.doc-number-allowlist.tsv"

# #4618: the scan floor. Duplicate/mismatch findings are legitimately 0 on a
# healthy tree, so the finding count cannot distinguish a real scan from an
# empty one — and this gate's true positives are additionally masked by only 4
# grandfathered allowlist rows, one `--update` from vanishing. The counts that
# CAN distinguish them are how many docs were opened and how many number claims
# were extracted. Both floors sit far below reality (54 spec docs) and far
# above zero.
MIN_DOCS_SCANNED=20
MIN_CLAIMS=20

RAW="$(mktemp "${TMPDIR:-/tmp}/docnum.raw.XXXXXX")"
VIOL="$(mktemp "${TMPDIR:-/tmp}/docnum.viol.XXXXXX")"
STALE="$(mktemp "${TMPDIR:-/tmp}/docnum.stale.XXXXXX")"
trap 'rm -f "$RAW" "$VIOL" "$STALE"' EXIT

DOCS_SCANNED=0
CLAIMS_SCANNED=0
scan "$RAW"

if [ "${DOCS_SCANNED:-0}" -lt "$MIN_DOCS_SCANNED" ] || [ "${CLAIMS_SCANNED:-0}" -lt "$MIN_CLAIMS" ]; then
  echo "FAIL: SCAN FLOOR — scanned ${DOCS_SCANNED} doc(s) yielding ${CLAIMS_SCANNED} number claim(s)," >&2
  echo "      below the declared minimums of ${MIN_DOCS_SCANNED}/${MIN_CLAIMS} (MIN_DOCS_SCANNED / MIN_CLAIMS" >&2
  echo "      in scripts/check_doc_numbers.sh). A gate with no claims to compare" >&2
  echo "      reports '0 violations' and cannot fail (issue #4618)." >&2
  exit 1
fi

if [ "$MODE" = "seed" ]; then
  # One-time bootstrap for a repo that has never been mechanically checked
  # (mirrors check_line_cap.sh --seed / check_test_pointers.sh --seed). Refuses
  # to clobber hand-curated rows.
  existing_rows=0
  if [ -f "$ALLOWLIST" ]; then
    existing_rows="$(awk -F'\t' '$0 !~ /^#/ && NF>=2' "$ALLOWLIST" | wc -l | tr -d ' ')"
  fi
  if [ "${existing_rows:-0}" -gt 0 ]; then
    echo "check_doc_numbers --seed: refusing — $ALLOWLIST already has ${existing_rows} row(s). Remove it first if you really intend to re-seed." >&2
    exit 1
  fi
  {
    echo "# .doc-number-allowlist.tsv — grandfathered document-number allocation"
    echo "# violations (see scripts/check_doc_numbers.sh)."
    echo "# Format: <CODE><TAB><key>. CODE is one of:"
    echo "#   DUP-DOC          key = the DOC number claimed by more than one file"
    echo "#   DUP-ADR          key = the ADR number claimed by more than one file"
    echo "#   LABEL-MISMATCH   key = the path whose filename and self-label disagree"
    echo "#   CATALOG-DANGLING key = the DOC number of a catalog row with no such file"
    echo "#   NEXT-FREE        key = the already-claimed number README advertises as free"
    echo "# The message column is NEVER part of the key, so rewording a diagnostic"
    echo "# never silently un-suppresses a grandfathered row."
    echo "#"
    echo "# Ratchet: rows may only be REMOVED. The gate FAILS on any row that no"
    echo "# longer describes a real violation, forcing prompt cleanup. Prune with:"
    echo "#   scripts/check_doc_numbers.sh --update"
    echo "# Do NOT add rows by hand for new work — a new collision must be fixed by"
    echo "# picking a free number, not grandfathered."
    echo "#"
    echo "# Seeded $(date -u +%Y-%m-%d) by --seed: the first-ever run of this gate."
    echo "# These are pre-existing allocations, several tangled with open issues"
    echo "# (renumbering a live DOC-N rewrites every inbound reference), so they are"
    echo "# recorded here for an owner decision rather than rewritten by this gate."
    awk -F'\t' 'NF>=2 {print $1"\t"$2}' "$RAW" | sort -u
  } > "$ALLOWLIST"
  seeded="$(awk -F'\t' 'NF>=2 {print $1"\t"$2}' "$RAW" | sort -u | wc -l | tr -d ' ')"
  echo "check_doc_numbers --seed: wrote $ALLOWLIST with ${seeded} grandfathered row(s)."
  exit 0
fi

apply_allowlist "$RAW" "$ALLOWLIST" "$VIOL" "$STALE"

if [ "$MODE" = "update" ]; then
  if [ ! -f "$ALLOWLIST" ] || [ ! -s "$STALE" ]; then
    echo "check_doc_numbers --update: no stale allowlist entries to prune."
    exit 0
  fi
  prune_stale_from_allowlist "$ALLOWLIST" "$STALE"
  pruned="$(wc -l < "$STALE" | tr -d ' ')"
  echo "check_doc_numbers --update: pruned ${pruned} stale entr$([ "$pruned" = "1" ] && echo y || echo ies) from $ALLOWLIST."
  exit 0
fi

# ----- check mode -----
violations=0
if [ -s "$VIOL" ]; then
  while IFS=$'\t' read -r code key msg; do
    [ -n "${code:-}" ] || continue
    echo "FAIL: [$code] $msg" >&2
    violations=$((violations + 1))
  done < "$VIOL"
fi

stale=0
if [ -s "$STALE" ]; then
  while IFS=$'\t' read -r code key; do
    [ -n "${code:-}" ] || continue
    echo "FAIL: [$code] \`$key\` is allowlisted in .doc-number-allowlist.tsv but is no longer a violation — remove its entry (run --update)." >&2
    stale=$((stale + 1))
  done < "$STALE"
fi

grandfathered=0
if [ -f "$ALLOWLIST" ]; then
  grandfathered="$(awk -F'\t' '$0 !~ /^#/ && NF>=2' "$ALLOWLIST" | wc -l | tr -d ' ')"
fi

total=$((violations + stale))
if [ "$total" -gt 0 ]; then
  echo "doc-numbers: scanned ${DOCS_SCANNED} doc(s) / ${CLAIMS_SCANNED} claim(s); $grandfathered grandfathered, ${violations} violation(s), ${stale} stale allowlist entr$([ "$stale" = "1" ] && echo y || echo ies) — FAILED." >&2
  echo "Pick a free number (scan docs/specs/ and docs/adr/ — the catalog's 'next free' note is a hint, not authority; DOC-38 §4.1)." >&2
  exit 1
fi

echo "doc-numbers: scanned ${DOCS_SCANNED} doc(s) / ${CLAIMS_SCANNED} claim(s) (floors ${MIN_DOCS_SCANNED}/${MIN_CLAIMS}); $grandfathered grandfathered, 0 violations — OK."
exit 0
