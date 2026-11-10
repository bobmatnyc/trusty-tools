Changed
- `tm presence devices add` no longer prints the `INCOMPLETE (#8878 PR2b-2)` banner or the raw registration options. With `rp_origin` unset it fails before the Touch ID prompt with "enrolment via browser is not enabled"; otherwise it prints the enrol link. `register.start` no longer answers an `incomplete` field (#8878).
- `mpm.presence.approve` and `mpm.presence.verify` refuse every device approval until #10865 verifies the presence key's Keychain access list (owner ruling D24); enrolment is not affected (#8878).
