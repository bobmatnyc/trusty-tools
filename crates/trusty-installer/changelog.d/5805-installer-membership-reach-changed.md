Changed
- Membership reaches four more subcommands, not just `install`. A bare
  `tctl upgrade` now replaces the running installer along with the rest of the
  stack, and `--exclude-self` is the opt-out — before membership that flag
  matched nothing and did nothing. `tctl updates` reports the installer's own
  staleness, and `tctl status` lists it as a non-daemon row (`health: n/a`).
  The daemon-shaped subcommands — `start`, `stop`, `restart`, `stack health`,
  `stack doctor` — are unaffected: they filter on `daemon`, and the installer
  is not one (#5805).
- `tctl config` no longer forwards to the installer. Membership made the
  fan-out spawn `trusty-installer config --json`, which is `tctl config`, which
  enumerated the set and spawned it again — unbounded recursion on the
  documented bare "all members" form. The installer is listed as `skipped`
  instead, and naming it alone (`tctl config trusty-installer`) is a usage
  error rather than an empty success (#5805).
- The PATH-shadow check covers every binary a member places, not just the one
  probed for health. A stale `tctl` earlier on `$PATH` used to win every shell
  invocation while the install reported clear (#5805).
- A forwarded contract verb (`<binary> config --json`, `<binary> version
  --json`) gives up after 10 seconds. It ran with no deadline, so a member
  binary that never exits froze `tctl config` indefinitely with nothing on
  stdout (#5805).
- A tarball missing a binary the shared `installed_binaries` table lists is
  reported at warning level. The preview names every binary a member places, so
  a tarball shipping fewer used to over-promise and under-deliver silently
  (#5805).
