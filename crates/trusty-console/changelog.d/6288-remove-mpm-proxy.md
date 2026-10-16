Removed
- The `/api/mpm/*` reverse proxy and its `/proxy/mpm/*` alias. No client used them once every mpm client moved to the daemon socket; both now answer 400 (unknown daemon) and never dial the daemon (#6288).
