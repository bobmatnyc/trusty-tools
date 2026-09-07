Added

- `tm doctor` gained a `skill_reachability` check — the skill mirror of
  `agent_reachability`. It fails when a skill the framework's own source roster
  declares deployable reached no tier the harness reads, or when the copy that
  would load has frontmatter that does not parse, declares no `name`, or
  declares a `name` that disagrees with the name it is deployed under. Every
  other skill probe audits something inside the tiers it is handed —
  `skill_staleness` compares checksums, `skill_unmanaged` compares against a
  deploy ledger, `skill_project_tier` reports a retired duplicate — so none
  could fail when a rostered skill reached no tier at all, which is what the
  deployer did to directory-shaped skills for weeks while every presence-only
  probe stayed green (#4949). One skill deployed at two tiers warns instead,
  naming both paths and the copy to delete: only the higher-precedence copy ever
  loads, and bundled skills are user-tier only since the 2026-09-01 owner ruling
  (#6586). An empty roster or a tier that exists and cannot be listed reports
  UNKNOWN, never `Ok`. The check is read-only and removes nothing it reports
  (#4947).
