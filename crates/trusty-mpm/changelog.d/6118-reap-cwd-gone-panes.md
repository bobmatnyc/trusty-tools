Fixed
- The orphan-GC now reaps a managed, untracked pane whose working directory is
  confirmed gone, even when the pane still runs an agent rather than a bare
  shell. That is the class `reconcile` declines to adopt because the cwd does
  not resolve; nothing could act on them before and 478 accumulated as
  permanent zombies (#6118).
  - The evidence is positive only (ADR-0045): a path tmux never reported, or one
    the filesystem could not decide on, keeps the pane, and the kill still needs
    the same two consecutive sweeps an idle-shell orphan needs.
- The orphan-GC's "untracked active managed session — skipping" line is now
  logged once per session per 60 sweeps rather than every sweep — it was
  992,078 lines in 48 hours, 76% of the daemon log — with the per-sweep total
  kept in the sweep summary. Override with
  `TRUSTY_MPM_ORPHAN_GC_SKIP_LOG_EVERY` (#6118).
