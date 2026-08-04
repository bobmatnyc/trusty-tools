Fixed

- The graceful Python bootstrap's per-retry readiness-probe budget is now
  capped at 3x the configured `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (90 s at
  the default). It previously scaled by attempt number with no ceiling, so a
  raised `TRUSTY_PY_BOOTSTRAP_RETRIES` grew a single probe's budget without
  limit — attempt 100 would have held one live Python child on one probe for 50
  minutes — and a pathologically large base could panic on `Duration` overflow.
  A capped probe now says so in its timeout log line, so it is distinguishable
  from an uncapped one (#4125)
