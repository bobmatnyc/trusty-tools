# Statusline cost savings

## Cost savings

trusty-mpm spends real effort *not* sending tokens: it folds several instruction
sources into one compiled prompt, it diverts a bulk file read to a cheap worker
and brings back a summary, and it compresses gate output before an agent reads
it. The `💸` segment on the `tm` statusline is an estimate of what all of that
adds up to for the session you are looking at.

```
TM 1.5.18 ● | trusty-tools ⎇ main | @bobmatnyc | ✻you@example.com | Opus | ctx 41% | $12.40 | ⏳24% 📅41% | 💸~$0.36
```

It has two forms and one absence:

| Folded total | Segment |
|---|---|
| At or above one cent | `💸~$0.36` |
| Above zero, below one cent | `💸~5k tok` |
| Nothing recorded for this session | *the segment is not rendered at all* |

The leading `~` is the estimate marker. It is there to keep this figure visually
apart from the `$12.40` cost segment two positions earlier, which is Claude
Code's own billed total for the session.

### The two numbers are not subtractable

The cost segment counts dollars actually spent. The savings segment counts
dollars *not* spent on tokens that were never sent. Netting one against the
other produces a number that means nothing: the counterfactual session — the one
that read every file in full and carried every instruction source verbatim —
never ran. Read the savings figure as "this is roughly what the harness avoided",
not as a discount on the bill.

Two further reasons it is an estimate, both stated so you can discount it
yourself:

- **Bytes are converted to tokens at four bytes per token.** That is the
  conventional English-prose approximation, not a tokenizer run.
- **Cache reads are not distinguished from fresh input.** Some of the tokens a
  technique avoided would have been cache reads, which bill at a tenth of the
  input rate. On a cache-heavy session the figure overstates.

### `$0.00` is never rendered

A rendered `$0.00` cannot be told apart from "nothing was saved", and it states
a measurement that was never made. So a sub-cent total falls back to the token
form, and a session with no recorded savings omits the segment entirely. If you
do not see a `💸`, no producer has written anything for that session.

## The techniques, and what each one measures

`technique` is an open string in the ledger, so a new producer needs no schema
change. Today one producer ships.

### `instruction-compression`

Written once per session launch, at the point that writes
`INSTRUCTIONS-COMPILED.md`.

- **Source set** — every instruction body the composer read for the session: the
  nine bundled section sources, plus each named-section override body it read
  from the project's `CLAUDE.md`.
- **Compiled output** — the bytes of the prompt actually delivered.
- **Saved** — source set minus compiled output, at four bytes per token, priced
  at the session model's published input rate.

Both figures land in the row's `basis` string, so any row can be checked by
hand.

The composer also *adds* generated context that no source file contributes — the
live agent roster and the detected stack profile. A project that overrides
nothing therefore produces a compiled prompt LARGER than its sources, and
**no row is written**. That is the correct answer, not a bug: the fold removed
nothing, so there is nothing to claim. The row appears when a project's
`CLAUDE.md` genuinely replaces a bundled section with a shorter one.

### `divert` (once the shunt lands)

The shunt/divert hook routes a bulk read to a `claude -p` worker and returns a
summary. When it lands it appends a `divert` row per diversion:
`tokens_saved` is the file's token count minus the summary's, and
`cost_saved_usd` is that delta priced at the parent model, minus what the worker
itself cost. Nothing in this page changes when it does — the segment already
folds whatever rows it finds.

### Adding another producer

Any call site that can compute a before/after byte or token count appends a row
with its own `technique` string. `tm compress`, which already knows the input
and output size of every gate log it trims, is the obvious next one. No change
to the ledger, the fold, or the segment is required.

## The ledger

One append-only JSON-Lines file, at `~/.trusty-mpm/usage/savings.jsonl` — or
under whatever framework root your `--root` flag, `TRUSTY_MPM_ROOT`, or
`[standalone] root` config key resolves to. One object per line:

```json
{"ts":"2026-09-07T02:41:00Z","session_id":"trusty-tools-ec","technique":"instruction-compression","tokens_saved":5300,"cost_saved_usd":0.0159,"basis":"sources 47000 B - compiled 25800 B, at 4 B/token, priced at claude-sonnet-4-6 input $3/Mtok"}
```

Read it directly with anything that reads JSON Lines:

```bash
# every row for one session
grep '"session_id":"trusty-tools-ec"' ~/.trusty-mpm/usage/savings.jsonl

# the machine-wide total, by technique
jq -s 'group_by(.technique)
       | map({technique: .[0].technique,
              tokens: (map(.tokens_saved) | add),
              usd: (map(.cost_saved_usd) | add)})' \
   ~/.trusty-mpm/usage/savings.jsonl
```

There is no rollup file and no second writer. The total is folded at read time,
every time, straight from the rows — so what the status bar shows and what `jq`
computes cannot drift apart.

### What the fold refuses

A producer bug must not be able to put a wrong number on your status bar, so the
fold rejects three kinds of row and logs each rejection at `warn`:

- a line that is not valid JSON — a crash mid-write costs that one row, nothing
  else;
- `tokens_saved` at or below zero;
- `cost_saved_usd` at or below zero, or unreadable.

A rejected row contributes nothing. It can neither lower nor inflate the total.

### Timing

The segment redraws when Claude Code re-invokes its `statusLine` hook, on its own
render cycle. A saving recorded by a background diversion appears at the next
natural render, not the instant it happens.

## Configuration

None. There is no environment variable and no config key: the ledger is written
where the framework root already resolves, and the segment appears when there is
something to show.
