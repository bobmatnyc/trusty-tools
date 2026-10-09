Fixed
- A command that auto-starts the daemon (`query`, `list` and the others that wait for it) now starts it on the socket the command polls (#9214). With `TRUSTY_SEARCH_SOCKET` naming a non-default path and no daemon running, the spawned daemon bound the data-dir socket instead, and the command waited 60 s and failed with "did not become ready within 60s on socket <path>".
