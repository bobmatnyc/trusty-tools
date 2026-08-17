Added

- `--no-install` keeps the guided flow and `trusty-audit run` off the network,
  restoring the previous refuse-and-report behaviour exactly. `trusty-audit
  tools` never installs, with or without the flag, so a status read costs no
  download ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
- `tools::unsatisfied` decides whether anything needs downloading from the same
  three conditions the sweep's preflight refuses on — the file is present, the
  version record names it, and that version equals the pin. An already-satisfied
  set constructs no HTTP client, so repeated invocations re-download nothing
  ([#5797](https://github.com/bobmatnyc/trusty-tools/issues/5797))
