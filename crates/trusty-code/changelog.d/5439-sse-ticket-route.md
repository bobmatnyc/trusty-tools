Added

- `POST /auth/sse-ticket?path=<stream>` mints a single-use ticket that expires in 30 seconds, for browser `EventSource` clients — which cannot send a header, and for which putting the durable token in the query string would write it into every access log. Minting requires the credential, so a ticket is never a way in; it is refused for any path but the two SSE streams, and the ticket it returns opens a `GET` of that exact path and nothing else, so a ticket read from a trace log cannot be spent on `POST /rpc` ([#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439))
