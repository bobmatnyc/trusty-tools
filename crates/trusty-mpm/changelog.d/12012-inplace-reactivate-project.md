Fixed

- Bare `tm` relaunching a stopped session in place (`POST /api/v1/sessions/managed/{id}/reactivate`) now reactivates the decommissioned project the session binds to and unparks its search index, as spawn and resume do. A failure of either step is the route's error, and the session record is rolled back off `active` so the client does not exec (refs [#12012](https://github.com/bobmatnyc/trusty-tools/issues/12012))
- The in-place reactivate request now waits up to 60 s for the daemon's answer instead of 2 s, and the daemon bounds the project and index steps at 45 s, rolling the session record back off `active` when they fail or overrun (refs [#12012](https://github.com/bobmatnyc/trusty-tools/issues/12012))
