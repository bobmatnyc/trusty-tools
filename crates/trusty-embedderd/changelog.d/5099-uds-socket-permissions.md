Fixed

- **The UDS listener bound a world-readable socket**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `bind_uds_listener` called `UnixListener::bind` bare, so the socket was
  created at the process umask (`0755` under the common `022`) in a directory
  that was not narrowed either — despite ADR-0031 and ADR-0032 both resting
  their access-control argument on a `0600` socket. It now binds through
  `trusty_common::uds::bind_hardened` (`0700` directory, `0600` socket before
  the first accept), and `run_uds_accept_loop` drops any connection whose peer
  uid is not this process's own.
- Version bumped to 0.3.11: 0.3.10 is already published on crates.io and this
  PR changes `src/**`, so leaving it would turn main's version-parity workflow
  red (#4421, #3366).
