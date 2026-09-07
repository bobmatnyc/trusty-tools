Fixed

- The pinned-tool preflight now refuses a binary it cannot execute. Its three
  conditions — the file is there, this client verified a version for it, and
  that version is the engagement's pin — answered which binary would run, never
  whether it could. A pinned copy whose execute bit was lost to a `cp`, an
  archive extraction or a hand edit satisfied all three, so the run started and
  then failed once per repository at spawn with `Permission denied (os error
  13)`, a message naming neither the tool nor the file. The refusal is a new
  `AuditError::PinnedToolNotExecutable` naming the tool and the path and saying
  the mode is the problem, raised before the first repository is touched, so
  nothing is written when it fires. An operator's `TRUSTY_AUDIT_*_BIN` override
  already refused on the same condition; this is that check on the pinned path.
  Unix only — Windows has no execute bit, and spawn stays the arbiter there.
- The bundle-level `reports/report.json` now carries `trusty_audit_version`, the
  version of the binary that wrote it. It was the one generated artifact with no
  version stamp — the index's "Produced by" line and Versions table, the excerpt
  header and the error digest all already had one — so a recipient reading
  `report.json` alone could not say which build's counting rules produced the
  numbers. Additive: `generated_at` and `debt_rollup` are unchanged, and a
  reader that does not model the new field ignores it (issue #6139).
