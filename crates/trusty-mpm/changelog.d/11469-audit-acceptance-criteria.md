Added

- `tm issue audit` reads the issue body and adds an `acceptance criteria` row: PASS for a labelled `Acceptance criteria:` list or an `## Acceptance` / `## Acceptance criteria` heading (case-insensitive), otherwise WARN naming the TICKETING.md rule. WARN never fails the audit; making it fail after one release is a follow-up (#11469).
