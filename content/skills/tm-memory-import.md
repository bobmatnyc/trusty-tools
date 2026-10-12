---
name: tm-memory-import
description: Run a kuzu-memory import into trusty-memory with the refused-row review — the deterministic `trusty-memory import kuzu` runbook, the masked `--dry-run --export-refusals` export, the `import kuzu review` record, status and finish loop, and the `--review-ledger` apply, behind the READY FOR IMPORT / GO gate. The reviewer reads masked rows only.
user-invocable: true
metadata:
  version: "1.0.0"
category: pm-workflow
tags: [memory, kuzu, import, review, secrets, pm-recommended]
effort: medium
---

# tm-memory-import — kuzu Import with Refused-Row Review

`trusty-memory import kuzu` copies kuzu-memory stores into trusty-memory
palaces. Its secret screen refuses some rows. This skill adds one review pass
over those refused rows before the single import window
([#277](https://github.com/bobmatnyc/trusty-tools/issues/277),
[#8624](https://github.com/bobmatnyc/trusty-tools/issues/8624),
[#8625](https://github.com/bobmatnyc/trusty-tools/issues/8625)).

The steps run in this order. Do not skip one, and do not reorder them.

1. The deterministic runbook dry run.
2. The masked export: `--dry-run --export-refusals`.
3. The review loop: `review record`, `review status`, `review finish`.
4. The ledger preview, then READY FOR IMPORT, then GO.
5. After GO only: the real import with `--review-ledger <path>`.

Check the commands against `trusty-memory import kuzu --help` and
`trusty-memory import kuzu review --help` before you start. If a flag below
is missing from the installed binary, stop and report it.

## Rules

1. Read masked rows only:
   `~/.trusty-tools/kuzu-review/<run>/masked.jsonl`. That file is the only
   row text the reviewer sees.
2. Never read the raw store or any copy of it: no `memories.db`, no
   `.kuzu-memory` directory, and no `trusty-kuzu-import-*` temp directory,
   which holds the unmasked export while an import runs. No `cat`, `Read`,
   `sqlite3`, `strings`, `cp`, `tar` or Python opener on them. The importer
   opens the store itself; you only pass its path as an argument.
3. Never run `kuzu-memory` itself or build an export by hand. The only
   commands that touch a store are the `trusty-memory import kuzu` forms below.
4. Never call `tm secrets` or the `trusty-secrets` CLI, and never contact a
   live secrets server. This review needs neither.
5. Never copy masked text, a marker's neighbouring text, a row id or a full
   ledger key into an issue, a PR body, a commit message, a memory drawer or
   a chat message. Report counts, verdict labels and the run name.
6. Never stop the trusty-memory daemon before GO. Every step up to the ledger
   preview is a dry run and runs beside a live daemon.

A pm-guard deny for the store paths is planned under #8625. Until it ships,
these rules are the only barrier.

## 1. The Runbook Dry Run

The window has a host runbook. For #277 it is
`~/.trusty-tools/runbooks/277-kuzu-import.md`. Its first step is a dry run,
with the daemon still up:

```bash
env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --dry-run \
  --discover [--root PATH]... [--max-depth N] [--python PATH]
```

- Name the stores with `--discover`, `--root PATH` (repeatable), or
  `--from PATH` for one store (a `.kuzu-memory` directory or its
  `memories.db`). `--from` conflicts with `--discover` and `--root`.
- `--palace NAME` overrides the target palace and requires `--from`.
- A walk refuses to run while `TRUSTY_MEMORY_PALACE` is set, hence
  `env -u TRUSTY_MEMORY_PALACE`.
- `--python PATH` names an interpreter that can `import kuzu`. Use the
  interpreter the runbook names, never another program.
- `--bridge-timeout-secs SECS` bounds one store's export (default 1800).
- `--update` rewrites drawers whose memory changed since the last import.
  The first import does not pass it.

Compare the totals line with the runbook's expected counts. Stop and report
if they differ or any store failed.

## 2. The Masked Export

Run the same store selection with `--export-refusals`. The flag requires
`--dry-run`:

```bash
env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --dry-run --export-refusals \
  --discover [--root PATH]... [--python PATH]
```

It prints `· review export: N row(s) exported masked, M auto-refused (...)`
and `run directory: <path>`. The run name is the last path component, a UTC
timestamp plus 8 hex characters. The root `~/.trusty-tools/kuzu-review` is
fixed.

Rows of an auto-refuse rule are counted and never exported: `provider_prefix`,
`aws_key_id`, `private_key_block`, `jwt`, a secret-shaped memory id
(`secret_shaped_id`), and a refusal no span locates (`unlocated`). They can
never be admitted, by any verdict.

## 3. The Review Loop

Each `masked.jsonl` line carries `key`, `store`, `id`, `masked`, `spans` and
`filter_version`. In `masked`, each secret span is replaced by a marker
`⟦S<i>:<rule>:<len>:<cls>:<entropy>⟧`. `spans[i]` gives the same span's
`rule`, `len`, `cls` and `ent_tenths` (bits per char times ten). Judge a row
from the masked text and the span shapes. The secret itself is never in the
file.

Record one verdict per row:

```bash
trusty-memory import kuzu review record --run <RUN> --key <KEY> --verdict keep-refused
trusty-memory import kuzu review record --run <RUN> --key <KEY> --verdict admit
trusty-memory import kuzu review record --run <RUN> --key <KEY> --verdict admit-redacted \
  --op redact_span:<i> [--op delete_span:<i>] [--op drop_line:<i>]
```

| Verdict | Effect at import |
|---|---|
| `keep-refused` | The row stays refused. |
| `admit` | Imports the row verbatim, tagged `kuzu-review:override`. Allowed only under the carve-out below. |
| `admit-redacted` | Applies the `--op` edits, screens the result again, and imports it only if the screen now passes. |

- `--op` takes a span index `<i>` from the marker's `S<i>`, and only with
  `admit-redacted`. `redact_span:<i>` writes `[REDACTED:<RULE>]`;
  `delete_span:<i>` removes the span; `drop_line:<i>` removes its whole line.
  Each index once.
- The carve-out: verbatim `admit` covers a row whose every span is
  `mixed_case_alnum`, or `base64_blob` below 4.5 bits/char (`ent_tenths`
  under 45) or under 24 chars. Any other row needs `admit-redacted` or
  `keep-refused`.
- `record` refuses an admit for a row with an auto-refuse span, an `admit`
  outside the carve-out, a bad op list, a second verdict for one row, and any
  verdict on a sealed run. A refusal is final for that call; pick another
  verdict.
- When unsure, record `keep-refused`. A later re-run can import the row.

Check progress, then seal the run:

```bash
trusty-memory import kuzu review status --run <RUN>
trusty-memory import kuzu review finish --run <RUN>
```

`status` prints exported, reviewed (per verdict), pending, invalid and
unknown-key counts and whether the run is sealed. Bring `pending` and
`invalid` to 0 before `finish`. After `finish` nothing more can be recorded,
and pending rows stay refused.

## 4. Preview, READY FOR IMPORT, GO

Preview the sealed ledger with a dry run (the daemon stays up):

```bash
env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu --dry-run \
  --discover [--root PATH]... [--python PATH] \
  --review-ledger ~/.trusty-tools/kuzu-review/<RUN>/ledger.jsonl
```

An unsealed ledger, one changed after its seal, or one with an unreadable line
aborts the run before any store is read.

The gate (owner rulings, 2026-09-25):

1. The PM sends **READY FOR IMPORT** to the supervisor. It names the exact
   import command, the backup path and the measured downtime.
2. The supervisor answers **GO**. Nothing below happens without it.
3. After GO only, the assigned agent stops the daemon, backs up the palace
   registry, runs the import, restarts the daemon, and verifies health
   (`trusty-memory doctor`) and one recall.

This skill does not authorize stopping the daemon. GO does.

## 5. The Import (after GO)

Follow the runbook's stop, backup, import, check, restart and smoke steps. The
import step is the step-1 command without `--dry-run`, plus the sealed ledger:

```bash
env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu \
  --discover [--root PATH]... [--python PATH] [--bridge-timeout-secs SECS] \
  --review-ledger ~/.trusty-tools/kuzu-review/<RUN>/ledger.jsonl
```

A real run refuses to start while a daemon is running. The runbook names the
stop command: `trusty-memory service stop` for a launchd-managed daemon,
`trusty-memory stop` otherwise. Pass is exit 0 with `0 store(s) failed`. A
failed or partial store is safe to re-run; the import is idempotent and
imports only what is missing.

## Report

Report the run name, the export counts, the verdict counts from `review
status`, the sealed state, and each gate step's exit status. Report no row
content.
