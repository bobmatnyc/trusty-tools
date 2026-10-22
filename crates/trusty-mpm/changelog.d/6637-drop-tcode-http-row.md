Changed
- The TCP-listener allowlist no longer carries the `trusty-code` `serve/http.rs` row: `tcode serve` binds no TCP port since #6637 slice 2c, so `scripts/check_no_tcp_listeners.sh` now fails any TCP bind added to trusty-code source ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637))
