Fixed
- `tctl ui` now fails the launch when it cannot start `trusty-console serve` in its own session. The `setsid` result used to be discarded, so a failure left the console in `tctl`'s process group, where a terminal hangup or a group kill aimed at `tctl` reached it (#8783).
