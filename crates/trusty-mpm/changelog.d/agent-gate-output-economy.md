Added

- `BASE-AGENT.md` (synced to `trusty-code`) adds a "Never Directly Monitor a
  Declarative Process" rule: run gates in their quiet/filtered form
  (`--quiet`, `tail`/`grep` for the summary line, or `tm compress --tool
  "<tool-name>"`), re-run only the failing case with full output, and never
  `gh pr checks --watch`. Prompted by two agents burning 415k and 546k tokens
  streaming per-test and per-check-poll output directly. States explicitly
  that filtered output is still raw output — the evidence rule is unchanged
