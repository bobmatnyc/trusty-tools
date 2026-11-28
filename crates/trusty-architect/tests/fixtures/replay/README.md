# Replay fixtures (#12026)

Each `*.json` file is one scrubbed recording written by
`trusty-architect-replay record`. `tests/replay_tool.rs`
(`committed_replay_fixtures_pass_the_refusal_check`) loads every file here, so
a fixture holding a home path or a secret-shaped value fails the suite.

## level_lint

Sixteen runs of the Architect's `level_lint.py`, replayed in
`tests/replay_level_lint.rs` against `examples/replay_port.rs level_lint`
(the port, `trusty_architect::level_lint`; #12028 P1c retired the #12026
stand-in). The first three (`pm-strict-file`, `unknown-level-stdin`,
`engineer-quiet`) are #12026's and still replay clean against the frozen
Python reference; the other thirteen were recorded from it on 2026-11-27. No recording
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

Eleven runs of the frozen Python reference, the Architect's
`spec_drift_check.py`, recorded on 2026-11-27 with `--clock 1795784400` and
replayed in `tests/replay_spec_drift_check.rs` against
`examples/replay_port.rs`. Each recording passes the root as `--root {ROOT}`.
Every recording is the recorder's capture of the reference's real output;
none was written or edited by hand. The input of each is its `files`: a
curated sample checkout (manifest, spec, instruction files, skills,
references, codex docs, owner-queue items, soak log, improvement ledger)
built with sample names only. It is not a copy of any live tree.

The `-h` path is not recorded: the reference's help description names an
instance id, which the port omits. A unit test in `src/spec_drift/args.rs`
pins that one difference.

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
| `bad-arg-unrecognized`, `bad-arg-root-value`, `bad-arg-explicit` | argparse paths |

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

## stale-tasks, stalled_tasks, task_wait_check and il_timeline (#12028 P1d)

Forty-six runs of the frozen Python reference, recorded on 2026-11-27 and
replayed in `tests/replay_tasks.rs` against `examples/replay_port.rs`. Every
input is a sample store or ledger built with sample names only. The three
clock-dependent scripts are recorded with `--clock 1795784400`.

- `stale-tasks/`: the script itself, twenty-two recordings: text and `--json`
  rows (the 24-hour boundary at one second either side, a task never dated,
  parked over and under seven days, a factory task, a non-UTC offset, rank
  ties and a bool rank), `--hours`, `$DASHBOARD_ROOT`, an empty store, a
  missing store, a corrupt or non-object document, `items` that is a file,
  the other layout, the jsonl layout and its errors, a bad
  `$DASHBOARD_FORMAT`, and the argparse paths.
- `stalled_tasks/`: `driver.py [hook]`, ten recordings. The script's command
  line reads `local/owner-queue/../il` and `local/owner-queue/items` beside
  its checkout; the driver points both at the sandbox root and runs the
  `__main__` body (`hook` uses the hook's cap of two). A `load_state` error,
  a traceback in the script, is printed as its last line.
- `task_wait_check/`: `driver.py`, eight recordings; it points `main()` at
  the sandbox root and runs it unchanged.
- `il_timeline/`: `driver.py PROJECT=DIR... [-- EPOCH...]`, six recordings:
  two projects (rotated logs, a re-opened id, a parked task, ties, CRLF, the
  three time formats and a one-digit hour), an empty project, and the two
  inputs that raise.

Not recorded: a `goal` that is a list or dict and an `--hours` that
`timedelta` refuses end the script in a traceback (the port prints its last
line, exit 1); a tmux-resolved per-PM ledger directory needs `$TMUX_PANE`,
which the recorder clears. Regenerate with the scripts and their imports
(`dashboard_*.py`, `datg.py`, `ids36.py`, `il_remind.py`, `task_tag.py`) and
each `driver.py` (as `st_driver.py`, `tw_driver.py`, `tl_driver.py`) copied to
a sandbox directory `P`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay
$R record --clock 1795784400 --out $D/stale-tasks/happy-text.json --seed $S/q=q -- $P/stale-tasks.py --root '{ROOT}/q'
$R record --clock 1795784400 --out $D/stalled_tasks/cli-stalled.json --seed $S/local=local -- $P/st_driver.py
$R replay $D/stale-tasks/*.json -- <target>/debug/examples/replay_port stale-tasks
```

## state-archive, aliases and brief-id (#12028 P1e)

Eighty-six runs of the frozen Python reference, recorded on 2026-11-27 and
replayed in `tests/replay_writers.rs` against `examples/replay_port.rs`. These
scripts write files, so each recording's `writes` holds the bytes the run
wrote, and a clean replay proves them. Every input is a sample built with
sample names only.

- `state-archive/`: the script itself, nineteen recordings, always with
  `--state local/state.md --archive local/state-archive.md`: a move into a
  new archive, an archive with and without its head line, CRLF and
  `\u2028` line ends, `--dry-run` (a heading past 100 characters, non-ASCII
  byte counts), nothing to move, an empty file, no sections, the `NEXT IDS`
  refusal (with and without `--dry-run`), `--keep` negative, zero, past 64
  bits and abbreviated, and the argparse paths.
- `aliases/`: the script itself with `ALIASES_FILE=aliases.json`,
  forty-one recordings: `resolve` (longest alias, quiet aliases with and
  without `--all`, a `quiet` dict, an ambiguous alias, stdin with CRLF, the
  eight-match cap, blank text, a missing or unreadable table, a bad command
  line that still exits 0), `list`, `check` (every schema error, an email,
  same-kind and cross-kind sharing), `add` (a new file, a merge, a project, a
  host given `--github`, the check refusal, a `KeyError` that prints the
  help, a bad table, a `.tmp` path that is a directory) and the argparse
  paths.
- `brief-id/`: `driver.py`, twenty-six recordings with `--clock 1795784400`.
  `brief_ids.py` takes `REPO` from its own location, outside a sandbox; the
  driver points the module paths and the function defaults bound at import
  at the working directory and runs `brief-id.py` unchanged. Recorded:
  `next` (empty, mixed legacy and media ids), `assign` (new id with
  frontmatter, no H1, a marker the registry lacks or has, a path the
  registry knows, a fenced H1, CRLF), `find`, `list`, `check` and the
  argparse paths.

Not recorded: a run that ends in a traceback (a missing state or brief
file, a bad registry line, ids exhausted past `MBffff`, a table whose lists
are not lists); the port prints the traceback's last line and exits 1. A
run with the default paths reads beside the script, outside the sandbox;
`tests/writers_smoke.rs` covers those on the shipped binaries. Regenerate
with the scripts copied to a sandbox directory `P` and the driver copied
there as `bi_driver.py`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay
$R record --out $D/aliases/list.json --env ALIASES_FILE=aliases.json --seed $S/aliases.json=aliases.json -- $P/aliases.py list
$R record --clock 1795784400 --out $D/brief-id/check-clean.json --seed $S/local=local -- $P/bi_driver.py check
$R replay $D/brief-id/*.json -- <target>/debug/examples/replay_port brief-id
```

## il_agent (#12028 P1f)

Fifty-one runs of the frozen Python reference, the Architect's
`il_agent.py`, recorded on 2026-11-27 and replayed in
`tests/replay_il_agent.rs` against `examples/replay_port.rs il_agent`. Every
recording is the recorder's capture of the reference's real output; none was
written or edited by hand. Every input is a sample ledger (code `qx`) and a
sample R library, built with sample names only. The script reads no time, so
no recording needs `--clock`, and it writes nothing, so every `writes` is
empty.

`driver.py` is copied beside the frozen modules (`il_agent.py`, `ilio.py`,
`datg.py`, `task_tag.py`, `ids36.py`, `il_remind.py`) as `ia_driver.py`.
`dispatch` prints `python3 <HERE>/il_agent.py`, where `HERE` is the script's
own directory, outside the sandbox; the driver imports the script, sets
`HERE` to the working directory (the sandbox root) and runs its `main()`
unchanged. Each run sets `ILIO_PROJECT=il` and `ILIO_DIR=ilio`, the script's
own inputs.

| Recording | Case |
|---|---|
| `brief-full`, `brief-bare-id`, `brief-other-goal` | A cited D item and R reply, owed rows matched by full and bare ids, the withheld-items line, long and non-ASCII titles, a padded token |
| `brief-no-created`, `brief-goal-missing` | A task with no creation day; a goal the ledger lacks |
| `brief-stale-stamp`, `brief-closed`, `brief-unknown`, `brief-badid`, `brief-field`, `brief-no-code` | Every task refusal |
| `brief-rref-unknown`, `brief-rref-slots`, `brief-extra-field`, `brief-extra-unknown`, `brief-no-library` | Every extra refusal, an absent R library |
| `brief-empty-ledger` | An empty ledger |
| `brief-top-project`, `brief-sub-project`, `brief-datg-session`, `brief-legacy-dir`, `brief-state-dir` | `--project` on either parser, `$DATG_SESSION`, the legacy directory, `$DATG_STATE_DIR` |
| `dispatch-full`, `dispatch-plain`, `dispatch-reject` | The `IL:` line with quoted extras and `--project`, an empty `--ask`, a refusal |
| `handback-ok`, `handback-file-crlf`, `handback-last-block` | Expansion, pipes in the pulse, `--file` with CRLF, the last of two blocks |
| `handback-no-block`, `handback-empty-stdin`, `handback-write-field`, `handback-check-reject`, `handback-mismatch`, `handback-bad-expect`, `handback-no-stamp`, `handback-closed` | Every handback refusal, empty input |
| `handback-no-created`, `handback-pulse-fails` | No pulse: no creation day, a pulse the strict grammar refuses |
| `state-missing`, `state-empty`, `state-bad-json`, `state-bad-shape` | Ledger errors (exit 2) |
| `help*`, `bad-arg-*` | argparse paths |

Not recorded: a run with no `$ILIO_PROJECT` or `$ILIO_DIR` reads beside the
script, outside the sandbox; a missing `--file`, or a ledger row without a
field the script indexes, ends in a traceback, which the port reduces to its
last line; a tmux-resolved per-PM ledger needs `$TMUX_PANE`, which the
recorder clears. `tests/il_agent_smoke.rs` covers the checkout defaults on
the shipped binary and the tmux lookup through a `Tmux` fake. Without a
checkout the binary exits 1 when it needs a default; the script has none to
miss.

Regenerate with the modules and the driver copied to a sandbox directory
`P` and the sample trees in `S`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/il_agent
$R record --out $D/brief-full.json --seed $S/std/il=il --seed $S/std/ilio=ilio \
  --env ILIO_PROJECT=il --env ILIO_DIR=ilio -- $P/ia_driver.py brief qx-T01@1115 qx-D01 'R01-b sha=abc'
$R replay $D/*.json -- <target>/debug/examples/replay_port il_agent
```

The last line ends:

```
ok   tests/fixtures/replay/il_agent/state-bad-shape.json
ok   tests/fixtures/replay/il_agent/state-empty.json
ok   tests/fixtures/replay/il_agent/state-missing.json
```

PASS: all fifty-one recordings replay clean against the port (exit 0), and
`il_agent_recordings_replay_clean_against_the_port` reports
`test result: ok. 1 passed; 0 failed`.

## verify_claim (#12028 P1g)

Sixty-six runs of the frozen Python reference, the Architect's
`verify-claim.py` with `verify_claim_input.py` and `architect_config.py`,
recorded on 2026-11-27 and replayed in `tests/replay_verify_claim.rs`
against `examples/replay_port.rs verify_claim`. Every recording is the
recorder's capture of the reference's real output; none was written or
edited by hand. gh, git and curl are fakes declared in each recording, so
the replay also compares every command's argv. Inputs use sample names only
(`example-org/example-repo`, `example-two/second`, `sample-crate`, padded
hex SHAs). The script reads no time and writes nothing.

`driver.py` is copied beside the frozen modules as `vc_driver.py`. The script
reads `local/architect.json` under its own checkout, outside the sandbox;
the driver points `architect_config.load()` at the working directory (the
sandbox root), runs `main()` unchanged, and prints an uncaught exception as
its traceback's last line, which is what the port prints.

| Recording | Case |
|---|---|
| `pr-clearance-match`, `pr-merged-match`, `pr-mismatch` | Every PR claim matching and differing; a bare repo name, a digit-string `pr`, an upper-case SHA prefix; a version on a non-`published` event runs no curl |
| `pr-short-and-notsha`, `pr-gh-fails`, `pr-null-reply` | Short and non-hex SHAs; a gh error that is not a rate limit (no REST); a `null` reply |
| `pr-reply-list`, `pr-state-not-string` | Values the script cannot use: an `AttributeError` |
| `rate-limit-rest`, `rate-limit-rest-closed`, `rate-limit-rest-fails`, `rate-limit-rest-unexpected`, `rate-limit-check-runs-unexpected` | The REST fallback for the PR and its check runs: a match, a closed PR, too many check runs, a failed and an unexpected reply |
| `issues-match`, `issues-mismatch`, `issues-no-account`, `no-repo` | Issue states and reasons, a pull-request number, a failed read, an unmapped owner, no repo |
| `local-match`, `local-absent`, `local-mismatch`, `local-git-fails`, `local-git-broken`, `local-short`, `local-no-checkout` | Local-checkout claims, git failures, short SHAs, no checkout |
| `published-*` | crates.io: match, mismatch, a curl failure, a reply without `crate`, an invalid crate name |
| `ci-*` | The CI run: match, mismatch (a falsy conclusion), no claim, a gh failure |
| `offline`, `offline-local` | `VERIFY_CLAIM_OFFLINE=1`: gh and curl unchecked, git still runs |
| `config-*` | No `repos` key, no file, bad JSON, `repos` not an object, two gh dirs for one owner, a missing and a present `ARCHITECT_CONFIG` file |
| `badinput-*`, `usage-*` | Every `BADINPUT` message, invalid JSON, a missing file, an empty file and empty stdin, CRLF; the usage paths |
| `input-stdin`, `input-literal`, `no-checkable-claims` | The `-` and literal forms; an event with nothing to check |
| `guard-refusal` | A `gh api` path outside the read-only pattern: `PermissionError` |

Not recorded: a gh child's environment (the recorder logs argv only);
`tests/verify_claim_smoke.rs` proves on the shipped binary that a gh child
gets no `GH_TOKEN` or `GITHUB_TOKEN` and the owner's `GH_CONFIG_DIR`. An
unreadable or non-UTF-8 event file and undecodable command output end in a
traceback or a decode error the port words differently.

Regenerate with the three modules and the driver copied to a sandbox
directory `P`; each case seeds `local/architect.json`, a sample checkout
directory `work/example-repo` and `event.json` from a directory `S`, and
passes `--fakes`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/verify_claim
$R record --out $D/pr-clearance-match.json --seed $S/local=local --seed $S/work=work \
  --seed $S/event.json=event.json --fakes $S/fakes.json -- $P/vc_driver.py event.json
$R replay $D/*.json -- <target>/debug/examples/replay_port verify_claim
```

The last line ends:

```
ok   tests/fixtures/replay/verify_claim/usage-help.json
ok   tests/fixtures/replay/verify_claim/usage-no-args.json
ok   tests/fixtures/replay/verify_claim/usage-two-args.json
```

PASS: all sixty-six recordings replay clean against the port (exit 0).

## gh-session-track (#12087)

Twenty-six runs of the frozen Python reference, the Architect's
`gh-session-track.py` with `dashboard_store.py`, `dashboard_jsonl.py` and
`dashboard_schema.py`, recorded on 2026-11-27 with `--clock 1795784400` and
replayed in `tests/replay_gh_session_track.rs` against
`examples/replay_port.rs gh-session-track`. Every recording is the
recorder's capture of the reference's real output; none was written or
edited by hand. gh is a fake declared in each recording, so the replay also
compares the argv of all four queries. Every run passes
`--repo example-org/example-repo` and `--root {ROOT}/q`; sessions are
`alpha`, `beta-01`, `gamma-01`, `gamma-02` and `zeta`, and every input is a
sample built with sample names only.

The write arms are in the replay set (Architect ruling, option A): each
`write-*` recording's `writes` holds the store files the reference wrote,
and a clean replay proves the port wrote the same bytes. Two differences of
the Rust store are handled without a normalizer:

- The store lock `.lock` and the change log `changes*.jsonl`, which only the
  Rust store writes (LDG-2.8.3), are left out of the comparison on both
  sides by `replay::skip_written_file`, the same rule
  `tests/dashboard_golden.rs` uses.
- The Rust store restamps `updated` with its batch clock (#10678); the
  reference keeps the stored value. Every seeded document carries
  `updated` equal to the recorded instant (`2026-11-27T13:00:00Z`), so both
  write one value. `store_batch_restamps_updated_with_the_batch_clock` pins
  the restamp; a recording seeded with an older `updated` replays with one
  diff, on that line.

| Recording | Case |
|---|---|
| `dry-run-text` | Every chip state, `ws/` exact and `-NN` prefix matches, an ambiguous prefix, a crate label with no session, the top-six cut and title cap, the 24-hour and 7-day edges, a future and a null time, PR rows in the closed pages, nameless sessions |
| `dry-run-json`, `dry-run-abbrev` | `--json` (non-ASCII escaped), `--dry --js` |
| `dry-run-empty`, `dry-run-empty-json`, `dry-run-no-store` | Empty replies, an empty store, no store |
| `dry-run-jsonl` | `DASHBOARD_FORMAT=jsonl` |
| `gh-fails-pr-list`, `gh-fails-api`, `gh-invalid-json`, `gh-invalid-json-text` | gh failures: a long stderr cut at 200, the REST read, an empty and an invalid reply; nothing written |
| `store-corrupt-session`, `store-session-not-object`, `store-wrong-format`, `store-bad-format`, `store-corrupt-meta` | Store errors before the batch; nothing written |
| `write-one-session`, `write-multi-session`, `write-json`, `write-new-store`, `write-empty-replies` | The write arm: one session, three sessions with other `details` keys kept and an existing `meta/gh_track` (version 4), `--json`, a store that does not exist yet, a tracked session with no items |
| `write-batch-fails` | A batch that fails at its second op (an invalid doc id): exit 1, nothing written |
| `help`, `bad-arg-*` | argparse paths |

Not recorded: a version-conflict retry needs a second writer between the
read and the batch, which a recording cannot schedule; the reference cannot
conflict with itself. `gh_session_track_retries_a_conflict_then_writes` and
`gh_session_track_gives_up_after_three_conflicts_and_writes_nothing` cover
it on a tempdir `Store` (`DashError::Conflict` is the script's
`ds.Conflict`). A `jsonl` write reads `schemas/dashboard/` beside the
script, outside the sandbox. A run without `--repo` names the reference's
default repo, a login. The reference keys its sessions by a `set`, so a
`--json` plan of two sessions has no fixed order; the port orders by name.
A gh item of a shape the script cannot use ends in a traceback.
`tests/gh_session_track_smoke.rs` covers the checkout defaults, the gh
environment and the no-checkout refusal on the shipped binary.

Regenerate: lay a recording's `files` into a directory `S` and its
`fakes.stubs` into `S/fakes.json`, with the four scripts copied to a sandbox
directory `P` (never the live checkout):

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/gh-session-track
$R record --clock 1795784400 --out $D/write-one-session.json --seed $S/local=local --seed $S/q=q \
  --fakes $S/fakes.json -- $P/gh-session-track.py --repo example-org/example-repo --root '{ROOT}/q'
$R replay $D/*.json -- <target>/debug/examples/replay_port gh-session-track
```

## ruling_pack (#12028 P1i)

Twenty-three runs of the frozen Python reference, the Architect's
`ruling_pack.py`, recorded on 2026-11-27 with `--clock 1795784400` and
replayed in `tests/replay_ruling_pack.rs` against
`examples/replay_port.rs ruling_pack`. Every recording is the recorder's
capture of the reference's real output; none was written or edited by hand.
`trusty-memory`, `trusty-search` and `rg` are fakes declared in each
recording that reaches them. The `trusty-memory` fake answers JSON-RPC by
request id (an answer's `rpc` map) and logs each request line, so the
replay also compares the `initialize`, `notifications/initialized` and
`memory_recall` requests byte for byte. The key map is a sample
(`sample-app`, `second-app`, `bare`) seeded into the sandbox, with `root`
entries inside it; the live `local/project-keys.json` is never read.
The sample key map sets no CTO flag field, so no recording reaches the
`cto` palace; the 17 recordings that seed a key map were re-recorded on
2026-11-28 without that field (#12097). The `cto` arm reads its field name
from the key map's top-level `cto_flag_field`, and
`cto_palace_follows_the_configured_flag_field` covers it with a neutral
name.

`driver.py` is copied beside the script as `rp_driver.py`. The script takes
ROOT from its own location; the driver loads it unchanged with `__file__`
under the working directory, so ROOT is the sandbox root, and prints an
uncaught exception as its traceback's last line, as the port does. Every
recording but `write` passes `--keys {ROOT}/keys.json --no-write`; `write`
uses the default key map `{ROOT}/local/project-keys.json` and writes its
pack to `{ROOT}/local/packs/`, which the replay compares.

| Recording | Case |
|---|---|
| `hit-all` | Memory hits (a hit without `content`, a reply line with no id, a float id), search hits of every field shape, grep matches, no matches and a cut line, `--type merge`, `--checked`: HIGH |
| `recall-miss`, `identifiers` | Empty recalls and results; the `#N`, file and symbol patterns (Unicode digits, `.tsx`, `.json5`, `foo.py.bak`, generic words) |
| `recall-errors`, `recall-shapes`, `recall-no-result`, `init-error` | A JSON-RPC error (cut at 200), a reply text that is not JSON, an empty `content`, a non-dict `text`, no `result`, a failed `initialize`; search failures: rc 1, no JSON, bad JSON, extra data |
| `no-fakes` | No backend at all (`trusty-memory` and `rg` absent) |
| `no-root` | An entry with no palace, index or root |
| `cap` | A pack over 6000 characters, cut with the label block kept |
| `search-results-dict`, `search-results-int`, `recall-results-str` | A `results` the script cannot slice: a dict (`KeyError`, exit 2), an int (`TypeError`), a string whose characters have no `.get` (`AttributeError`) |
| `write` | The write arm, default key map, pack under the sandbox |
| `unknown-project`, `keys-*` | Exit 2 and the traceback lines for a missing, invalid and `projects`-less key map |
| `help`, `bad-arg-*` | argparse paths, abbreviations, `type=int` and `choices` |

Not recorded: a recall that times out waits 25 s per reply line, and a
closed memory pipe races the next request. `tests/ruling_pack_smoke.rs`
covers the shipped binary: the checkout default key map, the write under
the checkout's `local/packs`, the search run without `TRUSTY_INDEX`, and
the run without a checkout.

Regenerate with the script and the driver copied to a sandbox directory `P`
(never the live checkout), the sample key map and `proj/` in `S`, and each
case's fakes in `S/fakes/`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay/ruling_pack
$R record --clock 1795784400 --out $D/hit-all.json --seed $S/keys.json=keys.json --seed $S/proj=proj \
  --fakes $S/fakes/hit-all.json -- $P/rp_driver.py sample-app "..." --keys '{ROOT}/keys.json' --no-write
$R replay $D/*.json -- <target>/debug/examples/replay_port ruling_pack
```

## ids36 and dashboard_schema (#12029 P2a)

Thirty-three runs of the frozen Python reference (architect commit
`0fbbc81eed`), recorded on 2026-11-27 with `--clock 1795784400` and replayed
in `tests/replay_ids_schema.rs` against `examples/replay_port.rs`. Neither
module reads the clock, writes a file or starts a process, so every `writes`
is empty and no recording declares fakes. Every input is synthetic.

- `ids36/`: `driver.py`, copied beside `ids36.py` as `i36_driver.py`,
  twenty-seven recordings. `cli ...` runs the script's `__main__` block
  unchanged: `next` across the `f->g`, `k->m` (no `l`), `n->p` (no `o`) and
  `z->10` boundaries, `zz` wrapping to `01`, `00`, a hex-era `ff`, a value
  with one final newline (accepted: Python's `$`), every refusal (uppercase,
  `o`, `l`, short, long, empty, a quote, non-ASCII) and the usage paths.
  `table`, `enc`, `dec` and `next` call the functions: `enc` at `0`, `1155`,
  `1156`, `-1` and `2^63-1`; `dec` on every refusal shape.
- `dashboard_schema/`: `driver.py CASES.json`, copied beside
  `dashboard_schema.py` as `ds_driver.py`, six recordings. `keywords`
  exercises every keyword's error text, the top-level type of each JSON
  kind, nested `enum`/`const` equality, patterns (one final newline, a
  60-character cut), the goal invariants, `meta` ids and `instructions`.
  `load-errors` covers a missing file, a directory, three decode errors
  (CRLF included), an unknown keyword at each nesting point, unsupported and
  unresolved `$ref`s and an unknown `type`. `store` covers `validate_store`'s
  cross references with and without `goals`/`items`. `helpers` covers
  `schema_name`, `mode` and `architect_do_missing`.

Not recorded: a value the script cannot hash or call `.get` on (a list
`goal` in `goal_removals`, a non-dict store body) ends in a traceback; a
`watch_changes` document (the frozen script does not list the collection,
the Rust port keeps it for #10896); a schema pattern outside the Rust
subset, which the port refuses at load by design. Regenerate with the
modules and drivers copied to a sandbox directory `P` and each recording's
`files` laid into `S`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay
$R record --clock 1795784400 --out $D/ids36/cli-next-0f.json -- $P/i36_driver.py cli next 0f
$R record --clock 1795784400 --out $D/dashboard_schema/store.json --seed $S/schemas=schemas \
  --seed $S/broken=broken --seed $S/cases.json=cases.json -- $P/ds_driver.py cases.json
$R replay $D/ids36/*.json -- <target>/debug/examples/replay_port ids36
$R replay $D/dashboard_schema/*.json -- <target>/debug/examples/replay_port dashboard_schema
```

## dashboard_jsonl and dashboard_derive (#12029 P2b)

Seventeen runs of the frozen Python reference (architect commit
`0fbbc81eed`), recorded on 2026-11-27 with `--clock 1795784400` and replayed
in `tests/replay_jsonl_derive.rs` against `examples/replay_port.rs`, which
hands each to the library's own driver mirror (`dashboard::pyjsonl` and
`dashboard::pyderive`, `replay_cases`). Every store and document is
synthetic. No recording declares fakes: neither module starts a process.

- `dashboard_jsonl/`: `driver.py`, copied beside `dashboard_jsonl.py` as
  `dj_driver.py`, twelve recordings. `pure` covers `canonical` (sorting,
  escapes, floats, a 30-digit integer, `NaN` and both infinities refused, a
  repeated key), `render_line`, `render_file`, `sort_ids` (byte order, not
  UTF-16 order), `sha256` and `changed_keys`. `parse` covers every
  `parse_file` arm: blank lines, CRLF, each JSON error, a BOM, each shape,
  `sv` and `version` refusal, a duplicate id and three UTF-8 errors. `read`
  covers `read_coll`, `collections` (the change-log names, hidden files) and
  `read_changes`. `append` is the only recording that writes, under
  `append/` in the sandbox: a fresh log, an empty batch, `seq` continuing, a
  torn tail, invalid trailing lines, monthly rotation (torn or not, the
  first valid entry dating it, a non-string or missing `ts`, an existing
  target overwritten), a 20,000-byte line across the 8 KiB scan window, a
  BOM line, a negative `seq` and a blank log. `fsck` covers clean, damaged
  and missing stores. `cli-*` run the module's `fsck [ROOT]` command line.
- `dashboard_derive/`: `driver.py CASES.json`, copied beside
  `dashboard_derive.py` and `ids36.py` as `dd_driver.py`, five recordings.
  `predicates` covers the predicates, `decision_counts`, `tally` (with and
  without `interactive`) and the run windows (Unicode digits, a final
  newline, impossible dates). `ids` covers the base-34 id space, a full
  prefix and every retire arm; one case reads the frozen clock. `times`
  covers the activity times (offsets kept in `isoformat()`), `stamp_progress`
  (Python `==` across `1`, `1.0` and `True`), the stale lists, `goal_sessions`,
  `ordered` and `by_acted_on`. `pending` covers every owner-event kind and
  `request_queue`.

Not recorded: values Python cannot hash or iterate (a list `goal`, a `null`
`sessions`) end in a traceback; a lone surrogate cannot be held by the port;
`snapshot(store, colls)` needs a live `Store`. Regenerate with the modules
and drivers copied to a sandbox directory `P` and each recording's `files`
laid into `S`:

```sh
R=trusty-architect-replay; D=tests/fixtures/replay
$R record --clock 1795784400 --out $D/dashboard_jsonl/append.json --seed $S/append=append \
  -- $P/dj_driver.py append/cases.json
$R record --clock 1795784400 --out $D/dashboard_derive/ids.json --seed $S/ids.json=cases.json \
  -- $P/dd_driver.py cases.json
$R replay $D/dashboard_jsonl/*.json -- <target>/debug/examples/replay_port dashboard_jsonl
$R replay $D/dashboard_derive/*.json -- <target>/debug/examples/replay_port dashboard_derive
```
