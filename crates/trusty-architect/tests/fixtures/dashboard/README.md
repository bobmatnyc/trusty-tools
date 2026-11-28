# Dashboard store goldens (#10063 PR 1)

Goldens for `tests/dashboard_golden.rs`, written by `regen_dashboard.py`
with the prototype's own writer (`dashboard_store.py`). Do not edit them by
hand.

| Path | Content |
|---|---|
| `corpus.json` | Each case's op batches, the prototype's outcome per format (results, or error class and message), and the `derived` values over the final store: next, free and retire ids per prefix, the tally, its line, the footer at a pinned clock, and each document's schema errors |
| `<case>/dir/` | The `dir` store after the last batch (`.lock` excluded) |
| `<case>/jsonl/` | The `jsonl` store after the last batch (`.lock` and the change log excluded; the change log is PR 2's) |

Cases: `synthetic` (the prototype's fixture store), `live` (a sample of the
live Architect store), both scrubbed with `../ledger/scrub.py`'s `Scrubber`,
and `edge` (encoder edge values and every refusal class).
`dashboard_fixtures_are_scrubbed` fails on a `live` string that is not filler,
a timestamp, a synthetic id, `pm-<n>` or an enum token.

#12029 P2e: each case's `derived.tally` holds the frozen `dashboard-db.py
tally` (architect commit `0fbbc81eed`: `derive.tally` plus `running` and
`answers_waiting`) over the committed `<case>/dir` and `<case>/jsonl`
stores at `NOW`, which added `decisions` and `blockers` and moved `live`'s
`answered_unrelayed` from 31 to 24. The tally line and footer goldens
are unchanged. `tally_corpus.json` is the synthetic store
`tally_follows_the_python_rule_for_canaries_interactive_decisions_and_relays`
reads.

## Regenerate

From the repository root, with the prototype repository and the live store
on the host:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/dashboard/regen_dashboard.py \
  --proto <architect repo> --live <architect repo>/local/owner-queue \
  --deny-out <path-outside-the-repo>/deny.txt
DASHBOARD_PROTO_DIR=<architect repo> cargo test -p trusty-architect --test dashboard_golden
```

`DASHBOARD_PROTO_DIR` also turns on `prototype_schemas_load_and_fixtures_validate`
(schemas are read from the prototype until #10063 PR 3 vendors them). The
deny list holds real values; never commit it.
