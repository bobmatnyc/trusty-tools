Added

- Managed session records carry a `kind`: `ordinary` (the default for every
  existing record), `supervisor` (the Architect), `supervisor_aux` (its
  `-poll`/`-collector` helpers), or `unknown` for a value this build does not
  recognise. Every kind except `ordinary` is protected: the supervisor sweep
  and boot reconcile never auto-resume it, and the managed-session list and
  get endpoints report the kind (#8942).
- The daemon refuses to kill or signal a tmux session by name when an
  Architect launch sidecar names it (or its `-poll`/`-collector` helper), or
  when a live record of a protected kind carries it. It also refuses, with a
  warning naming the caller, when the sidecars or the session store cannot be
  read. A stale record that reuses the Architect's tmux name can no longer
  take the Architect down (#8935, #8942).
- Do not run a pre-#8942 daemon against a post-#8942 store: an older daemon
  drops the `kind` field on its next write, and it has no kill-by-name
  protection for the Architect.
