Added

- `tga tui` — an interactive terminal view over collection and correlation
  (#5197). Three panes: a repo picker over the existing config surface
  (`repositories[]` entries are selectable; `github.orgs` entries are listed as
  discovery sources), live per-repository progress while a run is in flight, and
  a correlation results view showing which commits linked to which board items
  and which did not. Built on ratatui 0.29 / crossterm 0.28 — the workspace
  versions, matching `trusty-common`'s `monitor-tui` feature — so there is one
  ratatui major in the build graph. Raw mode and the alternate screen are
  restored on normal exit, on error, and on panic.
- Zero inference is the default and is enforced: every view, the correlation
  pass, and the pull all run with no model configured and no API key present.
  `[c]` (and `tga tui --correlate-only`) runs the deterministic
  commit-to-board-item link pass with no network at all.
- `tga::core::progress` — an optional, non-blocking progress bus. Pipelines
  publish `ProgressEvent`s carrying stage, target, counters, and a terminal
  outcome; delivery is bounded and drop-oldest, so a slow or absent consumer can
  never stall or fail a pipeline. The bus is opt-in at every emit site
  (`ProgressBus::disabled()` is what all existing CLI paths pass and makes every
  emit a no-op), so CLI behaviour including the `indicatif` bars is unchanged.
  `ProgressAggregate` folds the event stream into renderable rows.
- `tga::collect::correlate_commits` — the deterministic commit ↔ board-item
  link pass, plus `tga::core::db::correlation` read views (`correlation_counts`,
  `correlation_rows`, `CorrelationFilter`). The pass links a commit only when
  its ticket key matches a `work_items` row that is already present; a key with
  no matching item is reported as a gap, never invented. Idempotent. The
  `work_items` / `commit_work_items` schema is unchanged.
- `CollectionPipeline::with_progress` attaches a bus to the per-repository
  collect loop. Every configured repository reaches a terminal event, including
  one that cannot be opened.

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

Changed

- `indicatif` now resolves through `[workspace.dependencies]` instead of a
  duplicate local `0.17` pin.
