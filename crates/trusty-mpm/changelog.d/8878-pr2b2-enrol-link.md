Added
- `[presence] browser_enrol` (default `false`, flipped after owner decision D24). With it on and `rp_origin` set, `tm presence devices add <name>` prints a one-time enrol link, `<rp_origin>/tools/presence/enrol/#<ticket>`, valid once for 10 minutes; trusty-console serves the page that finishes the enrolment (#8878).
- `mpm.presence.register.start` refuses a ticket that already enrolled a device, so a used enrol link opens no second registration. `mpm.presence.status` reports `enrol_origin`, `null` while `browser_enrol` is off (#8878).
