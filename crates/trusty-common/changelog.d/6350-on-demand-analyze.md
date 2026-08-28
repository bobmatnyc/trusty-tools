Added
- `uds::server::serve_until_idle` and `uds::server::IdleTracker` give a UDS
  service an idle-exit policy: the accept loop returns `ServeExit::Idle` once no
  connection has ANSWERED anything for the configured window. A bare
  connect-and-close liveness probe does not restart the window, so a status page
  polling a service cannot pin it resident (#6350).
- `uds::OnDemandAnalyze` is the single entry point every client uses to start
  `trusty-analyze` before dialling it. Its spawn gate keeps two concurrent
  callers in one process from starting two servers, and every failure — no
  binary, a refused spawn, a socket that never appeared — is returned rather than
  degraded to a success (#6350).
- `SupervisorConfig::with_detached` spawns children without `kill_on_drop` and
  keeps them out of the population map, so a short-lived CLI does not SIGKILL a
  server another client is mid-request against (#6350).
