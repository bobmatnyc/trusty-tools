Added

- The audit sweep now scans each repository's working tree for leaked
  credentials with `gitleaks detect --no-git` and writes every match into the
  report manifest's `[report].findings` under the `secrets` category, so the DD
  report's Secret Leakage section states what is in the tree instead of
  disclaiming it (#6077). This is a different question from the crate's existing
  `credential_scan`, which checks the operator's own outbound package.
  - The matched credential is NEVER written. Each row carries `file:line` plus
    `trusty_common::credentials::redact_secret`'s masked preview, produced at
    the parse boundary so no other function in the collector ever holds the
    value; `-v` is not passed to gitleaks, whose verbose stderr prints matches.
  - A provider's own credential format (an AWS key id, a private key) bands RED;
    an entropy-only `generic-api-key` match bands AMBER, because that is the
    rule a reader has to triage by hand.
  - Unlike the CVE and license legs this one reads no dependency manifest, so it
    runs against every repository in a sweep rather than only the Rust ones.
  - Fail-open, and never silently: a missing binary, a failed spawn, a non-zero
    exit with no report, an unreadable report, and an unwritable manifest each
    get a gap naming which it was, leading with the collector's own diagnosis
    rather than with the child's first stderr line (#6720). A scan that ran
    clean states what it did not cover — git history above all.
  - gitleaks' own report file is the one artefact holding the matched
    credentials unredacted. It is written into a 0700 subdirectory of a
    `tempfile::TempDir`, whose drop removes it on every return path — including
    the early returns a manual delete misses — and the mode is set by `mkdir(2)`
    rather than by a later chmod, so there is no window in which another local
    account can open the directory. `Run`'s `Debug` is hand-written for the same
    reason: a derived one would put the raw report and the child's stderr into
    any `{:?}`.
