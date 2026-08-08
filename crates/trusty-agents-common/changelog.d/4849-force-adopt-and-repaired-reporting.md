Added

- `skills::reconcile::force_adopt_bundled_skills` — re-stamps every bundled-named skill a deploy target declines to refresh, covering the MANAGED-but-hand-edited case `adopt_unmanaged_bundled_skills` cannot reach. Backs every file up under the caller's backup root first, and the bundled roster stays the only admission test, so an operator's own skill is never touched. Backs `tm reinstall --force` (see [#4849](https://github.com/bobmatnyc/trusty-tools/issues/4849)).
  - `skills::unmanaged::bundled_skill_dirs` exposes the shared directory walk both the unmanaged detector and the force pass need, so neither grows a second copy.
  - `agents::deployer::DeployResult` gains `repaired`, listing the framework-owned files rewritten by the [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) corruption branch. That branch previously only logged, so a recovered corruption was indistinguishable from an ordinary refresh in the deploy result.
