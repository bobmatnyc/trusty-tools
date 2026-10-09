Fixed
- The `http_client` loopback proxy tests no longer flake on the guarded
  client: their stub server reads the request head before it answers, sends
  `Connection: close`, and shuts down its write half, so the 200 can neither
  reach the client on an idle connection nor be lost to a TCP reset. Test-only;
  no library behaviour changes (#6575).
