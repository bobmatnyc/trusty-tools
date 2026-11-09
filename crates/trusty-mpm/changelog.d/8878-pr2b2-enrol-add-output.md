Changed
- `tm presence devices add` no longer prints the `INCOMPLETE (#8878 PR2b-2)` banner or the raw registration options. With `browser_enrol` off it fails before the Touch ID prompt with "enrolment via browser is not enabled"; with it on it prints the enrol link. `register.start` no longer answers an `incomplete` field (#8878).
