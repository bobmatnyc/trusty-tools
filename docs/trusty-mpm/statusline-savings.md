# Statusline token savings

## Token savings

trusty-mpm spends real effort *not* sending tokens: it folds several instruction
sources into one compiled prompt, it diverts a bulk file read to a cheap worker
and brings back a summary, and it compresses gate output before an agent reads
it. The `💸` segment on the `tm` statusline shows what percentage of tokens
that would otherwise have been sent, this session avoided sending.

```
TM 1.5.18 ● | trusty-tools ⎇ main | @bobmatnyc | ✻you@example.com | Opus | ctx 41% | $12.40 | ⏳24% 📅41% | 💸34%
```

It has one form and one absence:

| Folded total | Segment |
|---|---|
| At least one accepted row, with a percent to report | `💸34%` |
| Nothing recorded for this session, or every accepted row predates #7179 | *the segment is not rendered at all* |

### How the percent is computed

The percent is a **session share**: how much of everything this session sent —
what it actually spent, plus what the harness avoided — did the harness avoid.

```
percent = tokens_saved / (session_actual_tokens + tokens_saved)
```

`tokens_saved` is every accepted ledger row's `tokens_saved`, summed across the
session. `session_actual_tokens` is a cumulative counter the `tm statusline`
compaction tracker keeps per session, alongside the `ctx 41%` segment's own
state, in `~/.trusty-mpm/statusline/<session_id>.json`. It exists because the
`statusLine` hook's raw `total_input_tokens` figure resets to a small number on
every auto-compaction — reading it directly would understate the session and
make the percent swing for reasons that have nothing to do with anything the
harness saved. The tracker instead folds each pre-reset reading into a running
base the moment it detects a drop, so `session_actual_tokens` only grows,
across any number of compactions in the session.

Before the first `statusLine` tick lands for a session — no compaction tracker
state yet — the segment falls back to the ledger-only ratio this feature
originally shipped with: `round(100 × tokens_saved / tokens_before)`, where
`tokens_before` is each row's own pre-saving token count. That fallback
produces the identical `💸<N>%` shape; nothing in the rendered segment
distinguishes which formula ran.

It is still an estimate, for the same two reasons as before:

- **Bytes are converted to tokens at four bytes per token.** That is the
  conventional English-prose approximation, not a tokenizer run.
- **Cache reads are not distinguished from fresh input.** Some of the tokens a
  technique avoided would have been cache reads, which bill at a tenth of the
  input rate — irrelevant to the *percent of tokens avoided*, but relevant if
  you convert the figure to a dollar estimate yourself.

### `0%` is never rendered

A rendered `0%` cannot be told apart from "nothing was saved", and it states a
measurement that was never made. So a fold with `tokens_saved > 0` always
reports at least `1%` — even a true sub-0.5% ratio rounds up rather than down
— and a session with no recorded savings omits the segment entirely. If you do
not see a `💸`, no producer has written anything for that session.

Dollar figures have not gone away — they still live in the ledger's
`cost_saved_usd` field and in `tm`'s own reporting commands. The statusline
just no longer surfaces one, because a percentage reads at a glance in a way a
bare dollar amount does not: it needs no context about the session's spend to
interpret.

## The techniques, and what each one measures

`technique` is an open string in the ledger, so a new producer needs no schema
change. Today two producers ship.

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

### `divert`

Written once per successful bulk-read diversion. `tm hook --divert-check` blocks
an oversized read, `tm divert bulk-read` answers it on a cheap `claude -p`
worker, and the session gets a summary instead of the file.

- **Files** — the bytes the worker read, which the session therefore did not.
- **Summary** — the bytes of the answer the session did read.
- **Saved** — files minus summary, at four bytes per token, priced at the parent
  session's published input rate, **minus what the worker itself billed**.

That last subtraction is what makes the figure a net saving rather than a gross
one: the worker is cheap, not free, and its own reported cost comes straight out
of the delta. All three numbers land in the row's `basis` string.

A diversion that returned a summary no smaller than the files it read saved
nothing, and one whose worker cost as much as the avoided tokens were worth
saved nothing either. Both write **no row**. A fall-through — no worker on
`PATH`, a worker error, an error reported inside the worker's JSON — writes no
row either, because no diversion happened.

#### Where the parent model comes from

The price a `divert` row uses depends entirely on which model the parent session
is running — a diverted Opus read is worth five times the same read on Sonnet —
so the row records which of three sources named it, in `model_source`:

| `model_source` | Source | When it answers |
|---|---|---|
| `env` | `ANTHROPIC_MODEL` | Only when you have pinned a model through that variable. It is an explicit override, so it outranks the rest. |
| `statusline` | The id Claude Code sent on its `statusLine` hook payload | After the status bar has rendered at least once in the session. This is the model Claude Code is really running. |
| `config-fallback` | The chain that produces a session's `--model` flag | When neither of the above answered — before the first render, or in a session with no status bar. |

The middle rung is the one that makes the figure right. Claude Code exports no
model variable to a hook child and sends no model on the `PreToolUse` payload, so
the `statusLine` payload is the only place `tm` ever learns the session's real
model. `tm statusline` writes it to
`~/.trusty-mpm/usage/session-model/<session-id>` on the render that changes it,
and `tm divert` — a separate process — reads it back from there.

A row priced at `config-fallback` is a row priced at a guess: the config chain
has a Sonnet default and always answers. Writing one also logs a warning naming
the fallback. If you see `config-fallback` on rows from a session you know is
running Opus, the status bar had not rendered yet.

A model the price table does not recognise declines the row and logs a warning
rather than pricing it at a guessed rate.

### Adding another producer

Any call site that can compute a before/after byte or token count appends a row
with its own `technique` string. `tm compress`, which already knows the input
and output size of every gate log it trims, is the obvious third one. No change
to the ledger, the fold, or the segment is required.

## The ledger

One append-only JSON-Lines file, at `~/.trusty-mpm/usage/savings.jsonl` — or
under whatever framework root your `--root` flag, `TRUSTY_MPM_ROOT`, or
`[standalone] root` config key resolves to. One object per line:

```json
{"ts":"2026-09-07T02:41:00Z","session_id":"trusty-tools-ec","technique":"instruction-compression","tokens_saved":5300,"tokens_before":11750,"cost_saved_usd":0.0159,"basis":"sources 47000 B - compiled 25800 B, at 4 B/token, priced at claude-sonnet-4-6 input $3/Mtok","model_source":"launch-config"}
```

`model_source` names where the model the row was priced at came from. For a
`divert` row it is one of the three values in the table above; for an
`instruction-compression` row it is always `launch-config`, because that producer
runs at session launch, off the very chain that produced the session's `--model`
flag. Rows written before this field existed read back with it empty and still
count toward the total.

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

None. The feature adds no environment variable and no config key: the ledger is
written where the framework root already resolves, and the segment appears when
there is something to show. The one variable it reads, `ANTHROPIC_MODEL`, is
Claude Code's own — set it and `divert` rows price at that model instead of the
one the status bar observed.
