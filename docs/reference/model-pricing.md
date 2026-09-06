# Model Pricing — the one table, and how to override it

> Every USD cost figure in the workspace — the trusty-agents REPL statusline and
> Costs tab, the SM's per-call `LlmResponse.cost_usd`, and the #6872 usage ledger
> — resolves through `trusty_common::pricing`. This page is the schema, the
> resolution rules, and the operator override. Issue: #6875.

## Where the rates live

`crates/trusty-common/pricing.toml`, embedded at compile time with
`include_str!`, so a daemon prices a turn without reading a file that may not
exist. Three crates used to keep their own tables and two had gone stale;
CLAUDE.md's "Common entry point, clean domain demarcation" gives a cross-crate
capability exactly one implementation, and this is it. Adding a second table is
a defect, not a variant.

## Row schema

```toml
[[model]]
id             = "claude-sonnet-5"   # required — canonical model id
input          = 2.00                # required — USD per million tokens
output         = 10.00               # required
cache_write    = 2.50                # optional, default 0.00 — 5-minute-TTL write
cache_read     = 0.20                # optional, default 0.00
effective_from = "2020-01-01"        # required — ISO date
provider       = "anthropic"         # optional — anthropic | bedrock | openrouter
aliases        = ["claude-sonnet-5-preview"]   # optional
source         = "legacy-table"      # optional — provenance
```

- **`effective_from`** is what makes a dated ledger honest: `rate_for(id, day)`
  picks the row with the LATEST date at or before `day`. A day earlier than every
  row for an id is unpriced, not priced at the earliest row.
- Every shipped row uses `2020-01-01` as a **sentinel** meaning "the earliest
  usage this table prices". None of them records a historical rate change, and
  inventing launch dates would put fiction in the one file whose job is to be the
  audited number. When a vendor changes a price, add a SECOND row for that id
  carrying the real change date.
- **`cache_write = 0.00` / `cache_read = 0.00` is a recorded absence**, not a
  discount. It marks a row whose source table priced no cache buckets (the
  OpenRouter-routed OpenAI ids).
- **`source = "legacy-table"`** marks a rate carried over verbatim from a table
  this file replaced, because no currently published rate covers that id. Those
  numbers are reproduced exactly rather than re-derived.

## How a model id resolves

The providers pass the same model under four spellings. `rate_for` normalises
before looking up, so a row needs one alias per genuinely different id rather
than one per route:

| Spelling | Where it comes from |
|---|---|
| `claude-sonnet-4-6` | Anthropic direct |
| `anthropic/claude-sonnet-4-6` | OpenRouter |
| `us.anthropic.claude-sonnet-4-6` | Bedrock |
| `bedrock/us.anthropic.claude-sonnet-4-6` | the SM's provider router |

Normalisation lower-cases, drops every `<segment>/` routing prefix, then drops
leading dotted segments while what follows is an `anthropic.` / `claude…` id — so
`gpt-5.4-mini-20260317` keeps its version dot. After an exact key miss, the
LONGEST row id that is a `-`/`.`/`:`-delimited prefix wins, which prices a dated
or versioned snapshot (`claude-sonnet-4-5-20250929`, `claude-sonnet-4-5-v1:0`) as
its base model.

**An id no row claims resolves to `None`.** The tables this replaced answered
every unknown id with Sonnet-class rates, which is a confident wrong number
rather than a visible gap. Callers turn `None` into `$0.00` and report it once
per process through `pricing::warn_unknown_model_once`, so the missing row shows
up in the log.

## Operator override

Write the same schema to **`~/.trusty-tools/pricing.toml`**. `pricing::shared()`
— the process-wide table every consumer reads — layers it over the bundled rows
at first use.

```toml
# ~/.trusty-tools/pricing.toml
[[model]]
id             = "claude-sonnet-5"
input          = 1.60
output         = 8.00
cache_write    = 2.00
cache_read     = 0.16
effective_from = "2026-09-01"
```

- The override **replaces every key it claims, wholesale** — it owns that id's
  entire rate history, so a partial edit cannot leave a bundled row shadowing the
  operator's. Ids the file does not mention keep their bundled rows.
- A missing file is not an error. A file that does not parse is logged and the
  bundled table stands: refusing to price anything because one operator file is
  broken would be worse than pricing from the shipped rates.
- The path is `~/.trusty-tools/pricing.toml`, at the root of the operator tree
  rather than under a crate directory, because pricing is workspace-wide rather
  than any one crate's configuration. The per-crate convention
  (`~/.trusty-tools/<crate>/config.yaml`) is in
  [config-convention.md](config-convention.md).

## Updating a rate

1. Load `Skill(skill="claude-api")` and read the published per-model rates; never
   write a number from memory.
2. Edit `crates/trusty-common/pricing.toml`. A price CHANGE adds a row with the
   real `effective_from`; a correction to a wrong row edits it in place.
3. Update the literals in `crates/trusty-common/src/pricing_tests.rs`. Every rate
   is asserted as a literal precisely so a rate edit is a reviewed diff.
4. Run `cargo test -p trusty-common --features unconditional-only --no-fail-fast`,
   then the consumer suites (`trusty-agents`, `trusty-mpm`) — a rate change is a
   rung-4 change under CLAUDE.md's test ladder.
