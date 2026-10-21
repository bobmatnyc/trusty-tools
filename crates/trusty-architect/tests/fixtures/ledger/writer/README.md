# PM ledger writer parity corpus (#9758)

Goldens for `tests/ledger_writer_parity.rs`. `regen_writer.py` writes every
file here except this README and itself, by running the prototype's
`datg.py` on the scrubbed stores in `../pm-*/`. Do not edit them by hand,
and never regenerate them from the Rust.

Baseline: the Architect prototype's `scripts/datg.py`, SHA-1 prefix
`de5e81131979`.

Each store runs one chain of verbs through `datg.main` with a clock pinned
to 2026-10-21T10:MMZ (minute = step), prune and show two days later: add,
a refused add, update (escalate, answer), close, a batch (add, update,
session), a batch that crosses the 8 KB bar, prune `--dry-run`, prune and
show. `pm-over-refuse` prunes itself under the 16 KB bar on its first
write; `pm-over-warn` refuses a write over it.

| File | Content |
|---|---|
| `corpus.jsonl` | one line per step: ops, exit, stdout, bytes appended to `log.jsonl` and `archive.jsonl`, `state.json` length and `v` |
| `<store>.state.json` | the store's `state.json` after its last step |

All strings are filler from the scrubbed stores or written by the script.

## Regenerate

From the repository root, with `<prototype-dir>` the prototype's `scripts/`:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/ledger/writer/regen_writer.py <prototype-dir>
git diff --exit-code crates/trusty-architect/tests/fixtures/ledger/writer
```
