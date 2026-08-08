Fixed

- A repository whose weekly walks failed reported `Completed` — "ok, 1 commit" —
  to the progress bus. Per-week failures (a `collect_window` error, a
  `collection_runs` lookup or record failure) reached `stats.errors` and the
  terminal event was emitted unconditionally, so `Outcome::Failed` was
  unreachable from that path. It now reports `Failed` with the error count and
  the first message, still exactly one terminal event per repository.
- `tga tui` had no surface at all for a collection error: the run summary read
  only `commits_collected`. It now reads `collected N commit(s), M error(s); …`,
  which is what the `Finished` status line shows.
- Quitting `tga tui` mid-run abandoned the worker silently — it is a detached
  thread nothing joins, so `q` / `Esc` / `Ctrl-C` cut an in-flight fetch or
  per-week write off at process exit with status 0. A quit during a run now
  takes a confirming second press, and says so.
- The pre-walk progress event tagged position-among-repositories onto the
  repository's own row, so a large repo mid-walk displayed "4/5" as if it were
  80% through itself. Nothing emits intra-repo progress, so the row is now
  "in flight, size unknown" until its terminal event; the how-many-repos
  roll-up stays in the stage header.
- The first `tga tui` frame against an unmigrated database rendered corrupted:
  the migration log lines printed before the alternate screen showed through
  ratatui's diff-rendered output and persisted across redraws. The TUI now
  clears the screen before its first draw.
- `tga tui`'s screen was corrupted for the entire duration of every `[r]`
  pull+correlate, permanently — three writers reached the terminal behind
  ratatui's back, and its diff renderer never repaints a cell it believes
  unchanged, so the garbling survived every later redraw and lasted the whole
  session. All three are now suppressed or rerouted for exactly as long as the
  TUI owns the screen, and each keeps its previous behaviour on every non-TUI
  path:
  - the revwalk's `indicatif` spinner (a 100 ms steady tick for the length of
    the walk) draws to `ProgressDrawTarget::hidden()` whenever a progress bus is
    attached, via the new `GitCollector::with_progress`;
  - the collect pipeline's `println!` / `eprintln!` operator lines — the
    per-week `Collected W31 2026: …` line among them — are published to the bus
    as `Collect` detail events instead;
  - the `tracing` subscriber's writer becomes an in-memory capture for the
    `tui` subcommand only, drained once per render tick into the ACTIVITY pane,
    so warnings that used to be written over the frame are now read in it. This
    matters at the default level with no `RUST_LOG` and no `-v`: a bare
    worktree whose `origin` is not fetchable emits the fetch-failure `WARN`.
    Every other subcommand keeps the byte-identical stderr path, which is what
    keeps stdout clean for MCP framing.
