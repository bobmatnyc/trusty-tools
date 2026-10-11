Security
- Every `/tools/secrets/` response carries `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`, so no other site can frame the Secrets page (#9067).
- A client that is not on loopback (a tailnet peer) gets a fixed 403 page for `/tools/secrets/` and never the page itself, until S3b lifts it (#9067).
- The secrets routes accept `localhost:PORT` on the console's bound port as the console's own Host and Origin, so the page works when opened at `http://localhost:PORT/tools/secrets/`; every other host name is still refused (#9067).
