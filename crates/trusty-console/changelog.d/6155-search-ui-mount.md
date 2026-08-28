Added
- Serve the trusty-search dashboard at `/tools/search/`. The console embeds its own copy of the SPA and injects `window.__SEARCH_BASE__ = /api/search/`, so every API call — chat included — rides the existing reverse proxy instead of the search daemon's own HTTP origin. The Search card links to it.
- `make -C crates/trusty-console search-ui` rebuilds and re-stamps that bundle from `crates/trusty-search/ui`; `scripts/check-ui-bundle-freshness.sh trusty-console` now checks it alongside the console's own.

Fixed
- The reverse proxy streams an upstream response instead of collecting it first. Collecting never returned for Server-Sent Events, so `/status/stream` and `/reindex/stream` delivered nothing until the 30-second request timeout fired. A request asking for `text/event-stream` also now uses a client with a read timeout in place of that whole-request deadline.
