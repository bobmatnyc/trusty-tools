Fixed
- The graceful python bootstrap scales its readiness-probe timeout by host load the same way the embedder supervisor scales its startup probe (`1 + load1/ncpu`, capped by `TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX`), plus 5 s, so a cold python start on a loaded host is no longer cut off before the supervisor's own probe decides it (#10207). The timeout log line reports the scaled value.
