Changed

- Version bumped 0.5.3 → 0.6.0. The crate's four #5626 breaking changes
  (`SkillManifest::load`, `skills::unmanaged::unmanaged_bundled_skills`,
  `skills::reconcile::preview_unmanaged_bundled_skills`, and
  `agents::tier_audit::audit_agent_tier` all became fallible; `TierAuditError`
  is new and public) shipped in an unpublished 0.5.3 patch bump. For a
  `0.y.z` crate the MINOR position is the breaking position, so this bump
  moves the crate to the version its own API change requires.
  crates.io's latest published version is still 0.5.2 — nothing is yanked
  or republished by this change.
