Added
- With `[presence] rp_origin` set, `tm presence devices add <name>` prints a one-time enrol link, `<rp_origin>/tools/presence/enrol/#<ticket>`, valid once for 10 minutes; trusty-console serves the page that finishes the enrolment (#8878).
- `mpm.presence.register.start` refuses a ticket that already enrolled a device, so a used enrol link opens no second registration. `mpm.presence.status` reports `enrol_origin`, `null` while `rp_origin` is unset (#8878).
