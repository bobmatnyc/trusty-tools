Fixed
- `trusty-memory stop` with `TRUSTY_DATA_DIR_OVERRIDE` set no longer stops the live `com.trusty.memory` launchd unit. It stops only the daemon serving the override's own socket, identified by the kernel's peer pid and confirmed as a `trusty-memory` daemon in the process table. When no daemon can be proven to own the override's data dir, or the override is blank, it exits non-zero and signals nothing (#9140).
- `trusty-memory service stop` refuses to run while `TRUSTY_DATA_DIR_OVERRIDE` is set, because it boots out the live launchd unit (#9140).
