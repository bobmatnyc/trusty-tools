Fixed

- **The controller and message-bus sockets were world-readable**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `CtrlSocket::bind`, `CtrlSocket::bind_singleton`, and `MessageBus::start` each
  created `~/.trusty-agents/sockets/` with `create_dir_all` and then bound bare,
  so both the directory and the socket landed at the process umask (`0755`).
  All three now route through `trusty_common::uds::bind_hardened` (`0700`
  directory, `0600` socket before the first accept); `bind_singleton` hardens the
  directory *before* probing, so a takeover of a stale socket is narrowed too.
  The ctrl and bus accept loops drop any connection whose peer uid is not this
  process's own.
