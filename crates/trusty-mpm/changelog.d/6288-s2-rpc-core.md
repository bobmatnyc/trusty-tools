Added

- The daemon serves its health, doctor, error-list, bug-report, breaker, optimizer, overseer, LLM-chat, tmux and claude-config routes as JSON-RPC methods on the Unix socket, under `mpm.<family>.<verb>` names. HTTP still serves all twenty routes unchanged; each route now has one shared body both transports call. (#6288)
