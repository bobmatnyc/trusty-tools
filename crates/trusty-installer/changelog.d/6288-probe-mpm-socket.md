Changed
- `tctl status`, `stack health` and the install verify tail probe trusty-mpm with `mpm.health` on its Unix socket, not `GET /health` on 7880 or its `http_addr`. A stub on a recorded `http_addr` no longer reads as the daemon (#6288).
