Added

- The audit sweep now scans each Rust repository's `Cargo.lock` with
  `cargo audit` and writes every advisory into the report manifest's
  `[report].findings`, so the DD report states dependency CVE exposure instead
  of disclaiming it (#6075).
  - Fail-open, and never silently: a target with no `cargo-audit` installed
    gets a gap naming the binary and `cargo install cargo-audit`; a non-Rust
    target gets `cve-scan: no cargo-audit-equivalent for <language>`; a scan
    that ran clean states what its lockfile scan did not cover. A repository
    declaring no dependency manifest at all is a declared skip and says nothing.
