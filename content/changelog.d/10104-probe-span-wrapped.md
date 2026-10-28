Fixed
- The `version-control` agent's fallback line now reads "Fallback without `tm` (this path has no merge gate):"; "which" wrongly referred to `tm` ([#10104](https://github.com/bobmatnyc/trusty-tools/issues/10104)).
- The `secrets-manager` agent now checks `command -v tm` before it names `tm hook --pm-guard`, and says what to do when `tm` is absent, so the probe-then-degrade lint passes now that it reads wrapped spans ([#10104](https://github.com/bobmatnyc/trusty-tools/issues/10104)).
