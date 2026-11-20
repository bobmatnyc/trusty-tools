Added
- `poller::watchdog`: the Architect watchdog ported from `architect-watchdog.py` (dead, frozen, silent, ok; one relaunch per 30 min through `session-relaunch.sh --rc`, given up after two failures). It runs every cycle of `tm architect poll` (#9779).
- `poller::routines`: the D5r check raises `routine_stale` when a routine listed in `local/routines.json` has not run within twice its interval, and logs a failure left uncleared for 30 min as due a push. Nothing is sent; `ARCHITECT_PUSH_CHANNEL` ships unset (#9779).
- Shadow run (`Config::shadow`, `PollArgs::shadow`): writes only `inbox/shadow/` (alerts, state seeded from the live state, `shadow.log`), takes no lock, sends no keys, notifies nobody and relaunches nothing (#9779).
