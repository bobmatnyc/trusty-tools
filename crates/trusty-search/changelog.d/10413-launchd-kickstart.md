Fixed

- A CLI command that finds the daemon socket down no longer spawns its own `start --foreground` while launchd owns `com.trusty.search`. It runs `launchctl kickstart gui/<uid>/com.trusty.search` (no `-k`) when the unit is loaded and idle, waits when the job is running, and waits then errors naming the unit when the plist exists but the unit is not loaded (for example during a bootout window): up to 60 s. An isolated socket (`TRUSTY_SEARCH_SOCKET`, `TRUSTY_DATA_DIR`, `TRUSTY_DATA_DIR_OVERRIDE`) and an unsupervised host spawn as before.
