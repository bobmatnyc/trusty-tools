Fixed

- Bare `tm` relaunching a stopped session in place (`POST /api/v1/sessions/managed/{id}/reactivate`) now reactivates the decommissioned project the session binds to and unparks its search index, as spawn and resume do. A failure of either step is the route's error, and the session record is rolled back off `active` so the client does not exec (refs [#12012](https://github.com/bobmatnyc/trusty-tools/issues/12012))
