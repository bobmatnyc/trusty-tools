Fixed

- The agent and skill ownership ledgers no longer report an absence they never
  established (#5626, ADR-0045). `SkillManifest::load` returned the empty
  manifest for a missing file, a malformed document, `EACCES`, and any other
  I/O error alike, and every consumer reads the empty manifest as "trusty-mpm
  owns nothing here" — which let a deploy write over managed skills and record
  none of them. It now returns `Result<SkillManifest>`: only
  `ErrorKind::NotFound` yields the empty default, and every other failure is a
  `ManifestError` naming the ledger. `AgentManifest::load_checked` routes
  non-`NotFound` I/O errors to `ManifestLoad::Corrupt`, so
  `quarantine::sweep_locked`'s `CorruptLedger` refusal now covers an unreadable
  ledger as well as a torn one. `audit_agent_tier` returns
  `Result<Vec<MisplacedAgent>, TierAuditError>` so a tier directory it could not
  enumerate is no longer reported to `tm doctor` as scanned-and-clean.
