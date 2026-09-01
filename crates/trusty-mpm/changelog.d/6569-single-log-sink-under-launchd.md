Fixed
- The daemon and supervisor no longer write every log line twice. Both modes
  registered a stderr layer beside the rotated `~/.trusty-mpm/logs/trusty-mpm.log.*`
  file layer, and under launchd the plist's `StandardErrorPath` captured that
  stderr into a file launchd never rotates — 3.4 GB against 3.0 GB of rotated
  logs over one 48-hour window. The stderr layer is now skipped when a file
  layer is configured AND launchd positively reports it supervises this process
  (#6569).
  - A foreground run keeps stderr, so `tm daemon` in a terminal is unchanged.
    So does a run where launchd could not be asked: the sink is only ever
    removed on positive evidence, never on an unanswered question.
  - Supervision is read through `trusty_common::supervision`, the same bounded
    `launchctl list` PID match every other caller uses, rather than a second
    environment-variable heuristic.
  - Operators: the existing `~/Library/Logs/trusty-mpm/stderr.log` is not
    truncated by this change or by a restart, because launchd appends. Truncate
    or delete it once after installing this version.
