Fixed
- The reverse proxy streams an upstream response instead of collecting it first. Collecting never returned for Server-Sent Events, so `/status/stream` and `/reindex/stream` delivered nothing until the 30-second request timeout fired. A request asking for `text/event-stream` also now uses a client with a read timeout in place of that whole-request deadline.
