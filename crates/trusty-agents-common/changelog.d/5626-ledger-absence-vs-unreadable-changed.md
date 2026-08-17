Changed

- **Breaking (#5626):** `SkillManifest::load` returns `Result<SkillManifest>`,
  `skills::unmanaged::unmanaged_bundled_skills` and
  `skills::reconcile::preview_unmanaged_bundled_skills` return
  `Result<Vec<UnmanagedBundledSkill>>`, and
  `agents::tier_audit::audit_agent_tier` returns
  `Result<Vec<MisplacedAgent>, TierAuditError>`. `TierAuditError` is new and
  public. Callers must decide what an unreadable ledger means for them; an
  `unwrap_or_default()` reinstates the defect this release removes.
