Changed
- Auto-discovery runs on every start again, over the socket; it was withheld from a `--no-http` daemon (#9214).
- `trusty-search port` exits 1 naming the socket for every current daemon and no longer reads an old `http_addr` file; `dashboard` and `monitor web` fail naming the socket and trusty-console, which serves the search dashboard (#9214).
- `trusty-search doctor` warns when the running daemon still binds HTTP, which means an older build is running and needs a restart (#9214).
- `trusty-search service install` drops `TRUSTY_SEARCH_NO_HTTP` from the regenerated unit, and the unit carries no `--port` or `--no-http` (#9214).
