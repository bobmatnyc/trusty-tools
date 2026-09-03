Added

- The report manifest carries `[report].findings`, and a report that declares
  any renders a new Assurance Scans section — one table per collector category,
  worst band first, each row linked to its advisory (#6075). `trusty-audit`'s
  cargo-audit collector is the first producer; the license and secrets
  collectors of #6076/#6077 reuse the same key under a different `category`.
- The Security Posture disclaimer is narrowed rather than dropped: it still
  denies a SAST result, a license review, a secrets scan and a penetration
  test, and now points at Assurance Scans for dependency CVE exposure instead
  of denying that a CVE scan exists.
