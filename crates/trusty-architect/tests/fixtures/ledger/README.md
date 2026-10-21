# PM ledger goldens (#9756 P0)

Goldens for `tests/ledger_contract.rs`: scrubbed copies of four live
prototype `datg` stores, in the prototype's own writer form (compact
`json.dumps(..., ensure_ascii=False)`). `scrub.py` writes every file here
except this README and itself. Do not edit them by hand.

| Directory | Size class | Content |
|---|---|---|
| `pm-small` | under the 8 KB warn bar | session record with `boundary_at` and `ctx_use`; `opts` as `{l, rec}` objects |
| `pm-medium` | under the warn bar | session record with `boundary_at`; titles over 120 characters |
| `pm-over-warn` | over 8 KB | session record with `refs`; no items |
| `pm-over-refuse` | over the 16 KB refuse bar | legacy store with no session record or archive; `opts` as a string and as a string list; titles over 120 characters |

Each holds `state.json` and, where the store had them, the first lines of
`archive.jsonl` and `log.jsonl` (both log shapes: `t`/`ev` and `ts`/`kind`).

## Scrubbing

Every free-text string becomes filler of the same length, every ledger code
a synthetic `x?` code, every log session name `pm-<n>`. Enum values,
timestamps, numbers, counters and key order are kept, so the shape and the
size class are kept. `ledger_fixtures_are_scrubbed` fails on any string that
is not one of those forms.

## Regenerate

From the repository root, with each store directory on the host:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/ledger/scrub.py \
  --deny-out <path-outside-the-repo>/deny.txt \
  pm-small=<store> pm-medium=<store> pm-over-warn=<store> pm-over-refuse=<store>
TRUSTY_LEDGER_DENYLIST=<path-outside-the-repo>/deny.txt \
  cargo test -p trusty-architect --test ledger_contract
```

The deny list holds real values; never commit it.
