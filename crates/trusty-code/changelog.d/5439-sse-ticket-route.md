Added

- `POST /auth/sse-ticket` mints a single-use ticket that expires in 30 seconds, for browser `EventSource` clients — which cannot send a header, and for which putting the durable token in the query string would write it into every access log. Minting requires the credential, so a ticket is never a way in ([#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439))
