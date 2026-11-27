# Replay fixtures (#12026)

Each `*.json` file is one scrubbed recording written by
`trusty-architect-replay record`. `tests/replay_tool.rs`
(`committed_replay_fixtures_pass_the_refusal_check`) loads every file here, so
a fixture holding a home path or a secret-shaped value fails the suite.

## level_lint

Fifteen runs of the Architect's `level_lint.py`, replayed in
`tests/replay_level_lint.rs` against `examples/replay_port.rs level_lint`
(the port, `trusty_architect::level_lint`; #12028 P1c retired the #12026
stand-in). The first three (`pm-strict-file`, `unknown-level-stdin`,
`engineer-quiet`) are #12026's and still replay clean against the frozen
Python reference; the other twelve were recorded from it on 2026-11-27. No recording
needs `--clock`: the script reads no time.

| Recording | Case |
|---|---|
| `rules-corpus` | Every rule, URL masking, overlaps, Unicode `\w`/`\d`, `re.I` job names |
| `crlf-file` | `\r\n` and a lone `\r` in a `--file` (universal newlines) |
| `non-technical-args`, `cto-equals-quiet`, `abbrev-and-dashes` | TEXT arguments, `--level=cto`, `--lev`/`--s` abbreviations, `--` |
| `empty-stdin`, `empty-file` | Empty input |
| `bad-arg-*`, `help` | argparse paths |

Not recordable: a run without `--level` reads the store beside the script
(`<checkout>/local/owner-queue`), outside the sandbox, so the recorder refuses
it; `tests/level_lint_smoke.rs` covers that path on the shipped binary, and
the `technical_level` `read-*` recordings cover the read itself. A missing
`--file` ends in a traceback, which the port reduces to its last line.

Regenerate with the script and its imports (`technical_level.py`,
`dashboard_store.py`, `dashboard_jsonl.py`, `dashboard_schema.py`) copied to
a sandbox directory `P`, never the live checkout:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/level_lint
$R record --out $D/rules-corpus.json --seed corpus.md=corpus.md -- $P/level_lint.py --level pm --strict --file corpus.md
$R replay $D/*.json -- $P/level_lint.py
$R replay $D/*.json -- <target>/debug/examples/replay_port level_lint
```

## technical_level and architect_config

Both are libraries. Each directory holds `driver.py`, copied beside the
frozen Python reference module (and the modules it imports) at record time
(2026-11-27), that calls the
library and prints its answers; `examples/replay_port.rs` mirrors each driver
line for line. `tests/replay_readers.rs` replays them.

- `technical_level/`: `driver.py table | value V... | read ROOT`, twelve
  recordings: the tables, odd values, a read of a `dir` and a `jsonl` store,
  a missing store, key or valid value (all `pm`), and the store errors (a
  corrupt document, the other format, a bad `DASHBOARD_FORMAT`), which the
  driver prints as the exception class only: the store's own message texts
  differ by design (#10063).
- `architect_config/`: `driver.py ROOT`, twenty-seven recordings: a full file
  (repeated key, unknown keys, a big integer, path normalisation), defaults
  for a missing or blank file, every environment override, `ARCHITECT_CONFIG`
  present and missing, each JSON error class, every type and range refusal.

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/architect_config
cp $D/driver.py $P/ac_driver.py
$R record --out $D/full-file.json --seed full=r -- $P/ac_driver.py '{ROOT}/r'
$R replay $D/*.json -- <target>/debug/examples/replay_port architect_config
```

## ledger_spec_check (not ported)

`ledger_spec_check.py` imports `test_ledger_spec` from its own directory and
runs its Python `unittest` guards in process; the guards start `datg.py`,
`il_dashboard_sync.py` and `dashboard-db.py` by `sys.executable`. A recording
cannot capture that (fakes reach only a `PATH` lookup), and a Rust port has no
Python guards to run, so #12028 P1c leaves it unported.

## spec_drift_check

Twelve runs of the frozen Python reference, the Architect's
`spec_drift_check.py`, recorded on 2026-11-27 with `--clock 1795784400` and
replayed in `tests/replay_spec_drift_check.rs` against
`examples/replay_port.rs`. Each recording passes the root as `--root {ROOT}`.
Every recording is the recorder's capture of the reference's real output;
none was written or edited by hand. The input of each is its `files`: a
curated sample checkout (manifest, spec, instruction files, skills,
references, codex docs, owner-queue items, soak log, improvement ledger)
built with sample names only. It is not a copy of any live tree.

`daily-pass` and `daily-no-log` run the reference on a sample checkout shaped
like the daily input, with no orphan reference, so every check passes or is
pending. `drift-orphan-fail` adds one reference the manifest does not list and
reproduces the reference's current orphan-reference failure (INV-03).

| Recording | Case |
|---|---|
| `daily-pass` | The PASS path (PASS and PENDING only, exit 0) |
| `daily-no-log` | The same with `--no-log`: no writes |
| `drift-orphan-fail` | A reference not in the manifest (INV-03 FAIL); writes the ledger |
| `drift-many-fail` | Every check fails, CRLF and non-ASCII text included |
| `pre-phase-pending` | Before phase 1: no skill index, no owner queue, an unenforced sentence (WARN) |
| `missing-manifest`, `empty-manifest`, `manifest-key-errors` | Error and empty inputs |
| `bad-arg-unrecognized`, `bad-arg-root-value`, `bad-arg-explicit`, `help` | argparse paths |

Regenerate: lay a recording's `files` into a directory `S`, then, with the
script and the `dashboard_*.py` modules it imports copied to a sandbox
directory `P` (never the live checkout; use a path without a symlinked
component, `/private/tmp` rather than `/tmp` on macOS):

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/spec_drift_check
$R record --clock 1795784400 --out $D/daily-pass.json \
  $(for e in $(ls -A $S); do printf -- '--seed %s=%s ' "$S/$e" "$e"; done) \
  -- $P/spec_drift_check.py --root '{ROOT}'
$R replay $D/*.json -- $P/spec_drift_check.py
$R replay $D/*.json -- <target>/debug/examples/replay_port spec_drift_check
```
