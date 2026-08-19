Fixed
- The `tctl install` footer now reports a PATH-shadowed component. A member that
  installed and bootstrapped cleanly but is shadowed by a different, earlier
  copy of the same name gave `all_ok: false` and exit 2 under a footer reading
  `installed 1/1 required component(s)` with no error line. Shadowing was the
  last `all_ok` input the footer left out; the human summary and the exit code
  now agree on every input (#5812, completing #5806).
