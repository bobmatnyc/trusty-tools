Fixed
- The pm-guard secret rule now refuses `launchctl print`, `launchctl dumpstate`, `pm2 jlist`, `pm2 prettylist`, `pm2 env` and `pm2 describe|show|info`, which print a launchd or pm2 job's environment and its API keys into the transcript. `launchctl list`, `pm2 ls` and `pm2 logs` still run (#8756).
