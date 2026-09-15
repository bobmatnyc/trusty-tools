# Statusline token savings

## Token savings

trusty-mpm spends real effort *not* sending tokens: it folds several instruction
sources into one compiled prompt, it diverts a bulk file read to a cheap worker
and brings back a summary, and it compresses gate output before an agent reads
it.

**The `💸` segment measures TOOL-OUTPUT COMPRESSION** — the last two of those
three. It shows what percentage of the tokens this session would otherwise have
sent, rtk- and shunt-style interception avoided sending: the `compress` rows a
`tm compress` run writes, and the `divert` rows a bulk-read diversion writes.
Both are credited to the tool call that produced them, and a Bash call
compressed a moment ago moves the figure on the next render — every render
re-reads and re-folds the ledger, with no cache between them.

**The instruction fold is recorded separately and does not feed the segment**
(owner ruling 2026-09-14, #7867). It is one comparison per session launch, of
the compiled prompt against the corpus it was folded from — a different
measurement on a different clock, and summing the two moved a per-call figure
for a reason no tool call caused. Its rows stay on the ledger under the
`instruction-compression` technique; `tm doctor`'s `instruction_fold` row is
where you read them. `tm doctor`'s `tool_output_compression` row reports what
the segment itself folds — rows, tokens saved, and when the last one landed.

```
TM 1.5.18 ● | trusty-tools ⎇ main | @bobmatnyc | ✻you@example.com | Opus | ctx 41% | $12.40 | ⏳24% 📅41% | 💸34%/29%
```

It has one form and one absence:

| Folded total | Segment |
|---|---|
| At least one accepted `compress`/`divert` row, with a percent to report | `💸34%/29%` |
| Nothing recorded for this session, or every accepted row predates #7179 | `💸—` |

The first figure is the **latest row** — what the most recent `compress` or
`divert` saved on its own. The second is the **mean across this session's
rows** (owner ruling 2026-09-15, #8063).

### Why two per-row figures

Until #8063 the left figure was a whole-session share, and it read as a defect:
the owner saw `💸1%` while `tm compress` was cutting individual tool outputs by
~19 %. Both numbers were correct — an 18.9 % reduction on one `git diff` IS
about 1 % of everything a long session sends — but the badge answered a
question nobody was asking it. The 2026-09-15 ruling moves both figures to the
row scope: what did the last interception save, and what do they save on
average here.

Neither number pools rows into one ratio. Each accepted row is priced alone:

```
row percent = round(100 × tokens_saved / tokens_before)
```

`tokens_before` is that row's own pre-saving token count, so the figure is the
reduction the producer measured for that one tool call. The left figure is the
newest such row (the ledger is append-only, so file order is arrival order);
the right is the arithmetic mean of all of them for this session. Three rows at
20 %, 30 % and 40 % therefore show `💸40%/30%`, where a pooled ratio over the
same rows would show 27 %. **With one row recorded, both figures are that
row's** — the badge never renders half-blank.

A row contributes only when it has a denominator of its own: a row written
before `tokens_before` existed (#7179) is skipped rather than counted as a zero
that would drag the mean down. Other sessions' rows reach neither figure.

Neither figure needs a new file. Both are folded from the same
`~/.trusty-mpm/usage/savings.jsonl` on the same render, filtered by
`session_id`, using the same accepted-row rules — so the two cannot disagree
about which rows count, and nothing is written to the usage directory to
produce them.

**A zero fold gates the whole segment.** With no savings rows for this session
you see `💸—` and no number at all, even when other sessions on the ledger have
plenty.

### The session share, and where it still lives

The session share — `tokens_saved / (session_actual_tokens + tokens_saved)`,
the 2026-09-08 ruling on #7179 — is still computed, just not on the status bar.
`tm commit-trailers` reports it for a whole session, where the question "how
much of everything this session sent did we avoid" is the one being asked.
`session_actual_tokens` is a cumulative counter the `tm statusline` compaction
tracker keeps per session, alongside the `ctx 41%` segment's own state, in
`~/.trusty-mpm/statusline/<session_id>.json`. It exists because the `statusLine`
hook's raw `total_input_tokens` figure resets to a small number on every
auto-compaction; the tracker folds each pre-reset reading into a running base
the moment it detects a drop, so the counter only grows.

The badge is still an estimate, for the same two reasons as before:

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
— and a session with no recorded tool-output compression renders `💸—` instead.
If you see that mark, no `compress` or `divert` row has been written for the
session.

Dollar figures have not gone away — they still live in the ledger's
`cost_saved_usd` field and in `tm`'s own reporting commands. The statusline
just no longer surfaces one, because a percentage reads at a glance in a way a
bare dollar amount does not: it needs no context about the session's spend to
interpret.

## The techniques, and what each one measures

`technique` is an open string in the ledger, so a new producer needs no schema
change. Today three producers ship — two of which the segment folds.

| `technique` | Written | Feeds the `💸` segment |
|---|---|---|
| `compress` | per `tm compress` run | yes |
| `divert` | per successful bulk-read diversion | yes |
| `instruction-compression` | once per session launch | no (#7867) |

### `instruction-compression`

Written once per session launch, at the point that writes
`INSTRUCTIONS-COMPILED.md`. **This row does not reach the `💸` segment** — it is
a launch-time measurement, not what a tool call avoided sending. Read it through
`tm doctor`'s `instruction_fold` row.

- **Source set** — every instruction body the composer read for the session: the
  nine bundled section sources, plus each named-section override body it read
  from the project's `CLAUDE.md`.
- **Compiled output** — the bytes of the prompt actually delivered.
- **Saved** — source set minus compiled output, at four bytes per token, priced
  at the session model's published input rate.

Both figures land in the row's `basis` string, so any row can be checked by
hand.

The measurement is taken at launch, but the row reaches the ledger one step
later. The `tm` process that compiles the prompt runs before `claude` is
spawned, so it does not yet know the session id the segment folds by — Claude
Code exports that id into its own children only. It stages the row under
`~/.trusty-mpm/usage/pending-savings/`, and the session's first `tm hook`
`SessionStart` invocation, which does know the id, appends it. A launch whose
hook never fires leaves the staged file in place for the next one.

The composer also *adds* generated context that no source file contributes — the
live agent roster and the detected stack profile. A project that overrides
nothing therefore produces a compiled prompt LARGER than its sources, and
**no row is written**. That is the correct answer, not a bug: the fold removed
nothing, so there is nothing to claim. The row appears when a project's
`CLAUDE.md` genuinely replaces a bundled section with a shorter one.

That decline costs the project nothing an operator watches: the segment folds
`compress` and `divert` only. The launch records it once per project at `debug!`
— "not smaller than the instruction sources", with both byte counts, repeated
only when those counts move — and `tm doctor`'s `instruction_fold` row states it
on demand.

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

### `compress`

Written once per `tm compress` run that actually shrank its input — the filter a
gate chain or a bash command pipes its output through before an agent reads it.
This is what puts bash and tool output into the segment; before it shipped, the
`💸` figure covered prompts and diverted reads only.

`tokens_saved` is the input's token count minus the compressed form's, at the
same four-bytes-per-token divisor the other two producers use, priced at the
session's own input rate. The row's `basis` names the `compression_path` the run
took — `rtk_binary` or `native_fallback` — so the two mechanisms can be split
apart later off the ledger line alone.

A run whose output is under the size gate passes through unchanged and writes no
row. That case saved nothing, and a zero-saving row would still fold its
before-figure into the percentage and pull both it and the average down.

### Adding another producer

Any call site that can compute a before/after byte or token count appends a row
with its own `technique` string. No change to the ledger, the fold, or the
segment is required.

## The same numbers on the commit

A commit made in a tm session carries what that session spent, as git trailers
below the attribution footer:

```
feat(trusty-mpm): a thing (Refs #7074)

What changed, and why.

🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
Claude-Session: https://claude.ai/code/session_01…

Tokens-In: 1284431
Tokens-Out: 38902
Savings: 34%
Model: claude-opus-4-1-20250805
```

| Trailer | Where it comes from |
|---|---|
| `Tokens-In` / `Tokens-Out` | the session's own Claude Code transcript, folded at commit time |
| `Tokens-Window` | present only when the fold read a tail window rather than the whole transcript |
| `Savings` | the ledger, folded for this session across every technique — the commit records the session's total, where the `💸` segment folds `compress` and `divert` only (#7867), so the two can differ |
| `Model` | `~/.trusty-mpm/usage/session-model/<session-id>`, written by the statusline render |

A value with no source is left out rather than written as a zero, exactly as
the segment omits itself. A commit made outside a tm session gets no block at
all.

### Which commits get a block

Tokens are a property of one session's work on one message, so the footer
follows git's **commit source** — the second argument git hands a
`prepare-commit-msg` hook — rather than being written onto every commit
(#7249).

| Commit source | What happens | Why |
|---|---|---|
| `merge` | nothing written | git built the message from the merge's parents; the tokens of whoever ran `git merge` describe none of it |
| `squash` | nothing written | the message is assembled by git or GitHub from a branch's commits, each with its own figures |
| `commit` — cherry-pick, revert, `--amend`, `-c`/`-C` | the inherited block is **removed**, then this session's figures are written in its place | the message arrives carrying the block the ORIGINAL commit's session wrote; those numbers describe different work |
| `message` (`-m`/`-F`), `template`, and the empty source of an ordinary editor commit | written | the message is this commit's own |

Cherry-picking outside a tm session removes the inherited block and adds
nothing: no block at all is correct where another session's block is not. That
one case is why the hook runs `tm commit-trailers` even with no
`CLAUDE_CODE_SESSION_ID` set. With no `tm` on `PATH` nothing runs at all, and an
inherited block stays as it is.

Within a single source the stamp is idempotent — a re-run over a message this
session already stamped adds nothing, so the block never doubles.

### Why it is a separate paragraph

Git recognises a trailer block only as the message's **last paragraph, whose
first line is itself a trailer**. The attribution line is not trailer-shaped,
so adding these keys to that paragraph makes `git interpret-trailers --parse`
return nothing — including the `Claude-Session:` line already there. The block
therefore goes below it, after a blank line. Verified against git 2.54.

The stamper is a bundled `prepare-commit-msg` hook, installed into the
repository's effective hooks directory by the same installer as the `pre-push`
guard, and refused the same way when a symlink, a foreign hook, or a
`core.hooksPath` redirect says the slot belongs to somebody else. It places the
block above any trailing comment block and above a `git commit --verbose`
scissors line, and it never stamps twice — a re-run over a message this session
already stamped leaves it alone, and an amend, which git reports as the `commit`
source, replaces the block rather than adding a second one.

`TM_SKIP_COMMIT_STATS=1 git commit …` skips it for one commit. The hook exits 0
on every path: a missing `tm`, a session with nothing recorded, or a failure
inside `tm commit-trailers` all leave the message exactly as git wrote it.

### What it costs a commit

Git blocks while the hook runs, so the stamper is bounded twice over.

- **A byte cap on the read.** `tm commit-trailers` folds at most the last
  **8 MiB** of the transcript. A day-long session's transcript reaches hundreds
  of megabytes; without the cap every commit after that point would wait on the
  whole file. 8 MiB reads in tens of milliseconds and spans hundreds of
  assistant turns, so an ordinary session is never cut at all.
- **A wall-clock budget in the hook.** The stamper runs with a **2-second**
  budget and is killed if it outruns it; the commit then proceeds with no stats
  block. This is the second bound, for a slow read the byte cap does not
  cover — a stalled network mount, a machine under load. Set
  `TM_COMMIT_STATS_TIMEOUT` to a whole number of seconds to change it. The
  message file is written through a temp-file rename, so a killed stamper
  leaves either the original message or the stamped one, never a half-written
  file.

When the cap does cut, the two counts describe that window rather than the
session, and the footer says so on its own line:

```
Tokens-In: 1284431
Tokens-Out: 38902
Tokens-Window: last 8 MiB of a larger transcript
```

The window is a separate trailer rather than a suffix on the numbers, so
`Tokens-In` and `Tokens-Out` stay plain integers for anything parsing them
back. There is no running per-session total to fall back on — the store behind
the fold holds the transcript's path and nothing else — so narrowing the claim
is what the footer does instead of guessing at one.

### Where the token counts come from

The `statusLine` payload carries a dollar cost and a context-window size, never
an output-token count, so the transcript is the only place the pair exists.
`tm statusline` remembers the transcript's path per session — the same store
and the same write-only-when-changed rule as the model record — and
`tm commit-trailers`, a separate process, folds the file at commit time.

That fold counts one message ONCE. Claude Code writes one transcript line per
content block and repeats the turn's identical `usage` object on each, so a
three-block turn appears three times; the fold dedupes on `message.id`.
Tokens-in is `input_tokens + cache_creation_input_tokens +
cache_read_input_tokens` — what was actually sent, where `input_tokens` alone
would exclude the cached prefix that is most of a long session's input.

## The ledger

One append-only JSON-Lines file, at `~/.trusty-mpm/usage/savings.jsonl` — or
under whatever framework root your `--root` flag, `TRUSTY_MPM_ROOT`, or
`[standalone] root` config key resolves to. One object per line:

```json
{"ts":"2026-09-07T02:41:00Z","session_id":"trusty-tools-ec","technique":"instruction-compression","tokens_saved":5300,"tokens_before":11750,"cost_saved_usd":0.0159,"basis":"sources 47000 B - compiled 25800 B, at 4 B/token, priced at claude-sonnet-4-6 input $3/Mtok","model_source":"launch-config"}
```

`model_source` names where the model the row was priced at came from. For a
`divert` or a `compress` row — both resolve the price the same way — it is one
of the three values in the table above; for an
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
