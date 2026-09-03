Added

- The audit sweep now reviews each Rust repository's dependency licenses with
  `cargo-deny list` and writes every copyleft, unlicensed, or unrecognised term
  into the report manifest's `[report].findings` under the `license` category,
  so the DD report states license/IP exposure instead of disclaiming it (#6076).
  - Strong copyleft (AGPL, GPL, SSPL, OSL, EUPL) bands RED; weak copyleft
    (MPL, LGPL, EPL, CDDL) and any license the policy table does not recognise
    band AMBER; a crate declaring no license at all bands RED. A crate offering
    any permissive term is cleared, so a `GPL OR MIT` dual license is not a
    finding.
  - Fail-open, and never silently: a target with no `cargo-deny` installed gets
    a gap naming the binary and `cargo install cargo-deny`; a non-Rust target
    gets `license-review: no cargo-deny-equivalent for <language>`; a non-zero
    exit or unreadable output names which it was; a review that ran clean states
    what it did not cover. A repository declaring no dependency manifest at all
    is a declared skip and says nothing.
