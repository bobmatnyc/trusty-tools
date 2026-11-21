Fixed

- `tm architect poll` and the seeded `fleet-poll.py` (started by `start-fleet-poll.sh`) now take their cycle length from `poll_interval_s` in `local/architect.json` (default 60 s) when `--interval` is not given, so every start path logs the same interval. Before, each path carried its own default. The seeded `local/architect.json` names the key.
