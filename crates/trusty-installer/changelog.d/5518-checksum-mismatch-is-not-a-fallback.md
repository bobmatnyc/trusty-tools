Fixed

- A prebuilt download that fails SHA-256 verification is no longer reported as
  "prebuilt unavailable" and silently rebuilt from source. `download::Outcome`
  now carries a distinct `ChecksumMismatch` variant, and `tctl install`,
  `tctl upgrade`, and `tctl self-update` all abort with an error naming both
  digests and the artifact URL instead of falling back to `cargo install`
  (#5518).
- `tctl install` no longer files an optional member's failed checksum under
  "skipped (optional, no prebuilt for this platform)" — in the checklist row or
  in the summary footer. An integrity failure now fails the run and its exit
  code regardless of whether the member is required.
