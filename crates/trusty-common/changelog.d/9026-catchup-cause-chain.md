Fixed
- The catch-up digest's warning for a failed `memory_list` prints the whole cause chain (a timeout, a refused socket file, a dead socket) and no longer calls every failure "could not reach trusty-memory" (#9026).
