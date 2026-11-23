Added
- `ta ask <instance> <question…>` asks an assistant instance and prints the answer. It starts the instance's daemon (`ta ask-daemon <instance>`) when none is running and reuses one that is (#11650).
- The daemon serves one Unix socket per instance at `<trusty-agents data dir>/trusty-agents-<id>.sock`. The bind is a singleton: a live daemon is never replaced, a stale socket file is taken over, and racing starts end with one daemon. The socket is 0600 in a 0700 directory, and a connection from another uid is refused (#11650).
- Asks run one at a time through the existing ask engine, so the turn log, replay cap, timeout and cancel rules are unchanged. A client that disconnects mid-ask cancels that ask (#11650).
- The daemon exits when its spawner has released it and no connection is open, with no timer and no polling. It removes its socket on exit and serves `agents.health` on the well-known `trusty-agents.sock` while it runs (#11650).
- An unknown instance, a daemon that fails to start (with the path of its log) and a protocol version mismatch are each reported as a distinct error (#11650).
