#!/usr/bin/env bash
# Keep Trusty Code's reconciled architecture and status claims from drifting.
#
# #5433 found that Trusty Code's specs and ADRs disagreed with the shipped code
# on four boundaries: native config root, event/channel authority, MCP topology,
# and canonical agent composition. The reconciliation catalog
# (docs/trusty-code/spec-adr-reconciliation.md) is the machine-readable record
# that closed those gaps; this gate keeps prose from silently reopening them.
#
# Runs in .github/workflows/doc-numbers.yml alongside check_adr.sh and
# check_doc_numbers.sh. Pure text scanning — no Rust toolchain needed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CATALOG="$ROOT/docs/trusty-code/spec-adr-reconciliation.md"
VISION="$ROOT/docs/trusty-code/vision-and-architecture-spec.md"
PARITY="$ROOT/docs/trusty-code/parity-spec.md"
ADR_CODE="$ROOT/docs/adr/0058-trusty-code-is-an-independent-product-owned-harness.md"
ADR_SOURCE="$ROOT/docs/adr/0059-canonical-agent-behavior-has-generated-host-adapters.md"
INDEX="$ROOT/docs/adr/INDEX.md"
DOC60="$ROOT/docs/specs/DOC-60-bus-based-agent-messaging.md"
DOC62="$ROOT/docs/specs/DOC-62-style-modes-coding-delegation.md"
EVENTS="$ROOT/crates/trusty-code/src/events.rs"

# The catalog table is the ownership/status/evidence map #5433 requires. Dropping
# a row is an architecture change, so the floor tracks the current row count.
CATALOG_ROW_FLOOR=17

errors=0

fail() {
  printf 'trusty-code-spec-check: %s\n' "$*" >&2
  errors=$((errors + 1))
}

require_file() {
  [ -f "$1" ] || fail "missing ${1#"$ROOT/"}"
}

require_text() {
  local file="$1"
  local text="$2"
  grep -qF -- "$text" "$file" ||
    fail "${file#"$ROOT/"} is missing required contract marker: $text"
}

for file in "$CATALOG" "$VISION" "$PARITY" "$ADR_CODE" "$ADR_SOURCE" "$INDEX" \
  "$DOC60" "$DOC62" "$EVENTS"; do
  require_file "$file"
done

if ((errors == 0)); then
  # Ownership and adapter direction are the non-negotiable reconciliation
  # boundary. Exact markers keep this gate simple and reviewable.
  require_text "$ADR_CODE" '**Status:** Accepted'
  require_text "$ADR_CODE" '**Trusty Code separates project configuration from private runtime state.**'
  require_text "$ADR_CODE" '`~/.trusty-code/` is the private root for mutable runtime state'
  require_text "$ADR_CODE" '**Delegated Code agents are daemon-owned durable tasks.**'
  require_text "$ADR_CODE" "**Trusty Code's client transport converges on UDS under ADR-0032.**"
  require_text "$ADR_SOURCE" '**Status:** Accepted'
  require_text "$ADR_SOURCE" '**One canonical authored source represents each instruction, agent, and'
  require_text "$ADR_SOURCE" '**Host layouts are deterministic generated adapters.**'
  require_text "$INDEX" '| [0058]('
  require_text "$INDEX" '| [0059]('

  # Every roadmap boundary named by #5433 keeps an owner/status/evidence row.
  for issue in '#2063' '#5425' '#5426' '#5428' '#5429' '#5430' '#5431' '#6637'; do
    require_text "$CATALOG" "$issue"
  done
  for status in 'Implemented' 'Partially implemented' 'Accepted, not complete' \
    'Designed, not wired generically' 'Accepted, not implemented' 'Not implemented'; do
    require_text "$CATALOG" "$status"
  done

  require_text "$VISION" '[spec/ADR reconciliation catalog](./spec-adr-reconciliation.md)'
  require_text "$PARITY" 'canonical Markdown/YAML behavior rendered through'
  require_text "$DOC60" 'Its `SessionEventEnvelope` remains the'
  require_text "$DOC62" 'Trusty Code has no style-mode type or dispatch branch.'
  require_text "$EVENTS" 'pub use trusty_agents_common::events::EVENT_LINE_PREFIX;'
  require_text "$CATALOG" 'legacy stderr prefix'

  # Trusty Code's client transport converges on UDS (ADR-0032 applied to Code by
  # the 2026-08-19 owner ruling on #6637). The catalog must keep naming the
  # surviving loopback TCP listener as interim, never as a granted exception.
  require_text "$CATALOG" 'the loopback TCP listener is interim'
  if grep -Eqi 'loopback( TCP)? HTTP (is|remains) (a |an |the )?(permitted|accepted|permanent)' \
    "$CATALOG" "$ADR_CODE"; then
    fail 'reconciled docs restored loopback HTTP as a permitted Trusty Code surface'
  fi

  # Validate every normative catalog row structurally.
  catalog_rows="$(awk -F'|' '
    /^## Normative statement catalog$/ { in_catalog=1; next }
    in_catalog && /^Paths above / { exit }
    in_catalog && /^\|/ && $2 !~ /^( Contract|---)/ { count++ }
    END { print count + 0 }
  ' "$CATALOG")"
  if ((catalog_rows < CATALOG_ROW_FLOOR)); then
    fail "normative catalog scan floor failed: found $catalog_rows row(s), expected at least $CATALOG_ROW_FLOOR"
  fi
  while IFS=$'\t' read -r contract status owner evidence verification roadmap disposition; do
    for pair in \
      "contract:$contract" "status:$status" "owner:$owner" \
      "evidence:$evidence" "verification:$verification" \
      "roadmap:$roadmap" "disposition:$disposition"; do
      value="${pair#*:}"
      [ -n "$value" ] || fail "normative catalog row has an empty ${pair%%:*} cell: $contract"
    done
    if [ "$verification" = "none" ]; then
      fail "normative catalog row has no verification target: $contract"
    fi
    if [[ "$evidence" != *'`src/'* ]]; then
      fail "normative catalog row has no Trusty Code source evidence or explicit absence path: $contract"
    fi
    if [[ "$roadmap" != *'#'* && "$roadmap" != "Deferred" ]]; then
      fail "normative catalog row has no issue or Deferred disposition: $contract"
    fi
  done < <(
    awk -F'|' '
      /^## Normative statement catalog$/ { in_catalog=1; next }
      in_catalog && /^Paths above / { exit }
      in_catalog && /^\|/ && $2 !~ /^( Contract|---)/ {
        for (i=2; i<=8; i++) {
          gsub(/^[[:space:]]+|[[:space:]]+$/, "", $i)
        }
        printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n", $2,$3,$4,$5,$6,$7,$8
      }
    ' "$CATALOG"
  )

  # Every source path cited as implementation evidence must exist. This caught
  # the stale src/serve.rs, src/http.rs, and src/build_state.rs claims in the
  # original reconciliation draft.
  while IFS= read -r evidence_path; do
    relative="${evidence_path#\`}"
    relative="${relative%\`}"
    [ -e "$ROOT/crates/trusty-code/$relative" ] ||
      fail "catalog cites missing Trusty Code evidence path: $relative"
  done < <(grep -oE '`src/[A-Za-z0-9_./-]+`' "$CATALOG" | sort -u)

  # The obsolete TOML claim was a concrete source-format contradiction.
  if grep -qF 'field of `<project>/.claude/agents/<name>.toml`' "$PARITY"; then
    fail 'parity spec restored the superseded TOML agent-source claim'
  fi

  if grep -Eq 'native (source|state|deployment).*`?\.claude/' \
    "$VISION" "$PARITY" "$CATALOG"; then
    fail 'maintained Trusty Code docs restored .claude/ as a native product root'
  fi
fi

if ((errors > 0)); then
  printf 'trusty-code-spec-check: %d violation(s)\n' "$errors" >&2
  exit 1
fi

printf 'trusty-code-spec-check: reconciliation contracts are consistent\n'
