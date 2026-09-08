Fixed

- The log-drain manifest cache directory (`<state_dir>/log-drain/<destination>/<cache_key>/`, under `~/.trusty-code/log-drain` and `~/.trusty-agents/log-drain`) is now created at `0700` instead of the umask-derived mode `create_dir_all` left it at. The directory records filenames and hashes of what has already left the machine, and gets the same tightening PR #7155 gave the sibling log directories in #6537. Refs #7158
