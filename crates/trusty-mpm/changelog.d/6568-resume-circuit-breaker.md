Fixed
- Auto-resume is now parked for a session whose runtime keeps exiting within
  seconds of its own auto-resume, instead of stopping and resuming it every
  60-70 seconds forever (2,170 stops against 2,128 resumes in one 48-hour
  window). The park is recorded as a `resume_flapping` stop cause, so every
  automatic resume path leaves the session down; an operator's own
  `tm session resume` clears it (#6568).
  - Tunable via `TRUSTY_MPM_RESUME_FLAP_WINDOW_SECS` (default 120) and
    `TRUSTY_MPM_RESUME_FLAP_THRESHOLD` (default 5); a zero window disables the
    breaker entirely.
  - An in-place reactivate (`tm`'s bare in-pane relaunch) clears the streak too,
    so an operator relaunching by hand cannot park their own session.
  - `tm session ls` marks a parked row `[resume-parked]` and `tm session info`
    carries the full reason, so a parked session is no longer indistinguishable
    from an idle stopped one.
