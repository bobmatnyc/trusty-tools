# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.5.0] — 2026-08-10

### Added

- `agents::tier_audit`: the shared classifier for agent files found OUTSIDE the canonical deploy tier. `agent_identity` resolves a file to the name the harness keys on (frontmatter `name:`, else the stem), `bundled_agent_names` builds the roster tm actually deploys from, `ownership_of` reads the three-state `TierOwnership` from the directory's ledger, `classify_tier_resident` returns `ShadowsBundled` / `StrandedFrameworkOwned` / `Custom` for one file, and `audit_agent_tier` does the read-only directory scan. A ledger entry recording the OPERATOR as owner outranks a bundled-name collision, matching `retract_framework_agents`, which never touches a user-owned file; `Custom` — a project's own agent — is never reported. An UNTRACKED file on a bundled name is NOT preserved the same way — it classifies as `ShadowsBundled`, since it is exactly what the quarantine counterpart exists to catch. Deliberately shared: `tm doctor`'s `asset_tier` probe reports what it returns and the quarantine counterpart moves it, so both must agree file-for-file ([#4442](https://github.com/bobmatnyc/trusty-tools/issues/4442)).
- `agents::quarantine` — moves an untracked project-tier agent file that SHADOWS a bundled agent name out of the way, reversibly (closes [#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448))
  - a file moves only when all FOUR gates agree: it resolves to a bundled name, the ownership ledger does not record it as the operator's, git does not claim it, and the file is trusty-mpm's own composer output. Each gate is independently fail-closed
  - `agents::vcs_claim` — gate 3. A repository that COMMITS a project-tier agent is declaring it, so `git ls-files` stands in for the `Origin::Project` declaration [#4443](https://github.com/bobmatnyc/trusty-tools/issues/4443) was going to provide. Three states, never a bool: "no repository" and "git could not be asked" have opposite safe answers. Neither git's exit code nor its message is trusted on its own — it exits 128 for every fatal condition, and it reports "no repository" for an unreadable `.git` exactly as it does for an empty directory — so absence must also be corroborated by a filesystem check that no ancestor carries a `.git`
  - `agents::agent_schema` — gate 4. claude-mpm, a separate live project, deploys into the same `.claude/agents` convention reusing trusty-mpm's exact filenames under a different schema. Identification is positive — the frontmatter key set rules a file OUT, and the composer's own base preamble in the body rules it IN — never the filename and never the file size
  - move-with-backup only. A verified byte-identical copy is taken before the original is renamed to an inert `.md.disabled` sibling. No code path calls `remove_file` or `remove_dir`, pinned by `never_deletes_on_any_path`
  - every examined file lands in exactly one of the report's moved / skipped / failed lists, and the receipt is rendered from that report — so a run that fails part way still records what moved and what did not
  - the receipt's restore command POSIX-single-quotes every path, so a filename carrying `$(…)` or backticks cannot execute when pasted, and the receipt itself is collision-protected so a same-second rerun cannot truncate the previous run's record
- `agents::deployer::retract_framework_agents` / `retract_framework_agents_filtered` — the inverse of a deploy, for a directory that is no longer a deploy destination ([#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)). Removes exactly the manifest-tracked, framework-owned (`Origin::Bundled`) files the deployer wrote, prunes their ledger entries, and returns a `RetractResult` naming what was removed and what was preserved. A file absent from the manifest (hand-placed) is never touched, a tracked entry with a user-owned origin is kept, a corrupt manifest is an error that removes nothing, and a retraction that empties the ledger deletes the manifest (and the directory, when empty) so the location returns to pristine. Checksum drift is deliberately not consulted: on a framework-owned file that means corruption, not ownership. The `_filtered` variant takes the same stem predicate `deploy_agents_filtered` does, so an operator-named agent scope can be honored.
- `agents::manifest::with_agent_manifest_lock` / `manifest_lock_path` — serialise a deploy directory's whole load-modify-save cycle across PROCESSES via an advisory `flock(2)` on a `.trusty-mpm-manifest.json.lock` sidecar (#4409). Needed because the agent deploy target became one machine-global directory shared by every concurrent session launch, sync-assets run, and `tm catalog apply`: two writers that each load before either saves silently drop each other's entries, and the files those lost entries described then fall into the deployer's untracked branch and are skipped from then on — #4408's freeze shape reached by a race. An in-process `Mutex` cannot help; same sidecar convention and `fd-lock` crate `trusty_common::json_rmw` uses for the identical hazard (#3502).
- `agents::manifest::Origin::is_framework_owned` — names the two install-ownership tiers so the deployer branches on declared ownership rather than on checksum mismatch alone (#4408).
- **`agents::metadata::AgentMetadata::agent_type` — the claude-mpm-format
  spelling of an agent's declared domain** (for
  [#4511](https://github.com/bobmatnyc/trusty-tools/issues/4511)). A deployed
  `.claude/agents/*.md` artifact that originated from claude-mpm declares
  `agent_type:` and no `role:` at all, so a consumer reading one through this
  read-only projection saw no domain whatsoever and had to fall back to a
  fail-closed default. `split_frontmatter` now parses the key (with the same
  unescape treatment as `role:`) and projects it alongside `role`, so one
  reader answers "what domain does this file declare?" for both artifact
  dialects instead of each consumer hand-rolling a second scan. The two
  spellings stay independent fields — which one wins is the CONSUMER's
  reviewed policy, and the value is a DECLARATION that must be translated
  before it reaches any authorization decision, never used verbatim
- Compose output is byte-identical: `agent_type:` is deliberately NOT merged
  across an `extends` chain nor re-emitted by `merge_frontmatter`, because
  this composer canonicalises on `role:` and emitting a second domain key
  would change the bytes of every deployed artifact for a value nothing in the
  compose path consumes. Dropping it on emit is exactly what happened before
  the field existed; `agent_type_is_parsed_but_never_emitted` pins it
- `skills::unmanaged`: the READ-ONLY detector for a bundled-named skill a deploy target does not manage. `unmanaged_bundled_skills` returns the skill directories under one deploy target whose stem names a currently bundled skill and which that target's `.trusty-mpm-skills-manifest.json` does not track, with the `SKILL.md` entry point and every `references/*.md` sibling enumerated, plus `UnmanagedBundledSkill::manifest_keys` deriving the exact keys `deployer::deploy_one_file` looks up. A stem matching nothing bundled is never returned — that is the operator's own skill, and the exclusion is what the tier system exists for. An empty roster reports nothing rather than condemning every deployed skill at once ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- `skills::reconcile`: the explicit, human-initiated repair half. `adopt_unmanaged_bundled_skills` copies every in-scope file under a caller-supplied backup root — mirroring its absolute path so two tiers holding a same-named skill cannot clobber each other, and appending a `moved-paths.log` line per copy — then records each file in the deploy target's manifest with the checksum of the content ALREADY ON DISK. That single act is the whole fix: the stem stops being classified project-custom by `tiers::list_project_custom_stems`, so the next `deploy_all_skill_tiers` plans it into `bundled_deploy` and the deployer's existing managed-and-unmodified branch refreshes it. Scope is decided only by `skills::unmanaged`, never re-derived, so report and repair cannot drift ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- Content similarity is deliberately NOT an adoption test. An untracked file byte-identical to a shipped version is indistinguishable from a customization that started from it, so no "adopt anything that looks like ours" predicate exists at any layer ([#4605](https://github.com/bobmatnyc/trusty-tools/issues/4605)).
- `skills::reconcile::force_adopt_bundled_skills` — re-stamps every bundled-named skill a deploy target declines to refresh, covering the MANAGED-but-hand-edited case `adopt_unmanaged_bundled_skills` cannot reach. Backs every file up under the caller's backup root first, and the bundled roster stays the only admission test, so an operator's own skill is never touched. Backs `tm reinstall --force` (see [#4849](https://github.com/bobmatnyc/trusty-tools/issues/4849)).
  - `skills::unmanaged::bundled_skill_dirs` exposes the shared directory walk both the unmanaged detector and the force pass need, so neither grows a second copy.
  - `agents::deployer::DeployResult` gains `repaired`, listing the framework-owned files rewritten by the [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408) corruption branch. That branch previously only logged, so a recovered corruption was indistinguishable from an ordinary refresh in the deploy result.

### Fixed

- `agents::deployer::retract_locked`'s corrupt-manifest error now points at a remedy that actually works for a corrupt workspace ledger: manually deleting `.claude/agents/.trusty-mpm-manifest.json` in that workspace, instead of `tm repair deploy`, which since [#4437](https://github.com/bobmatnyc/trusty-tools/pull/4437) repairs only the machine-global user-tier deploy and can never touch a per-workspace ledger. Message-only change; behavior is unaffected. (PR #4437 review comment [5137295142](https://github.com/bobmatnyc/trusty-tools/pull/4437#pullrequestreview-5137295142))
- `agents::manifest::atomic_write` stages through a per-process, per-attempt scratch path (`<file>.<pid>.<nanos>.tmp`) instead of a fixed `<file>.tmp` sibling, and removes the scratch file on a failed write ([#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)). A shared temp name is a corruption bug in its own right: two writers interleave into the one scratch file and `rename` publishes the mangled result — a torn manifest (surfacing as `AgentManifestCorrupt` in the spawn/resume gate) or a torn agent file. Survivable while every deploy target was per-workspace; not once the target is a single machine-global directory. Same reasoning and naming scheme `trusty_common::json_rmw` adopted after the identical fixed-`projects.json.tmp` corruption (#3502).
- `agents::deployer` now re-deploys a corrupted bundled agent instead of freezing it as user-modified (closes [#4408](https://github.com/bobmatnyc/trusty-tools/issues/4408)). A manifest entry whose origin is framework-owned (`Origin::Bundled`, the `InstallPolicy::Overwrite` tier) is refreshed from the bundle whenever its on-disk checksum drifts — a mismatch there means corruption or drift, never user ownership. Previously ANY checksum mismatch was read as "the user edited this file", which is unrecoverable for a bundled asset: corrupt content can never checksum-match again, so a 32-byte `v1` stub that replaced the real 25KB `rust-engineer.md` was permanently misclassified as a user edit and skipped by every subsequent deploy and by `tm validate --repair`, dropping the agent from the session roster for the life of the workspace. The user-owned tier is unchanged — an untracked file, and a tracked entry with `Origin::User`/`Origin::Registry`, are still preserved byte-for-byte. Each overwrite-on-mismatch repair logs the file, the expected and found checksums, and the on-disk byte count. ([#4419](https://github.com/bobmatnyc/trusty-tools/pull/4419)) ([`e8f30ef`](https://github.com/bobmatnyc/trusty-tools/commit/e8f30ef00bb3c7636c53182e88ee8a569acd6442))
- serialise the skill manifest's read-modify-write so concurrent deploys stop freezing skills nobody edited (closes [#4881](https://github.com/bobmatnyc/trusty-tools/issues/4881))
  - `deploy_skills_filtered` and the unmanaged-skill adoption now run their whole load-modify-save under a new `with_skill_manifest_lock` sidecar lock, the skill-side counterpart of the agent ledger lock added in #4409
  - `SkillManifest::save_merging` folds in any entries a writer that bypassed the lock published mid-run, instead of dropping them; it never fails or refuses after skill files are on disk, because bytes newer than their recorded checksum are exactly what the deployer reads as a hand-edit and skips forever
  - a manifest that exists but does not parse is no longer treated as an empty one — merging from that default would publish only the current run's entries and drop the rest
  - a mid-loop I/O failure during a deploy no longer skips the manifest save, so a skill written just before the failure stays tm-owned instead of freezing
  - the `flock` critical section is now one implementation (`with_ledger_lock`) shared by the agent and skill ledgers
- Skill deployment now accepts a directory-shaped skill (`<stem>/SKILL.md`), not only a flat `<stem>.md` (closes [#4949](https://github.com/bobmatnyc/trusty-tools/issues/4949))
  - the source scan tested `entry.file_type()?.is_file()`, so a directory-shaped skill was dropped with no warning on every deploy and the run still reported success
  - every file the skill carries — `metadata.json`, `references/**`, `scripts/**`, at any depth — now deploys and is recorded in the ownership manifest, so a multi-file skill is never half-tracked
  - a directory that carries no `SKILL.md` is reported by name in `DeployStats::skipped` and logged, instead of being skipped silently
  - `skills::tiers::list_source_stems` now calls the deployer's own scan rather than its own copy of the filter, so the planner and the deployer cannot disagree about which skills exist
- the skill tier-collision log no longer claims a winner it cannot deliver. It said "deploying the higher-precedence copy", which for a project-custom winner stated the opposite of what happens — Claude Code resolves skills personal over project, so a same-named copy under `$CLAUDE_CONFIG_DIR/skills` still beats the project-tier file. The line now scopes itself to deploy-time source precedence within one destination and names that `dest`

### Changed

<!--
  Note: the two entries below predate 0.2.1 (they reference #1959/#1968/#1750,
  the trusty-agents-common 0.1.3 publish) and were left stranded under a stale
  duplicate "## [Unreleased]" heading below the 0.2.1 section for several
  releases (issue #2793 pattern) instead of being folded into whatever version
  actually shipped them. Merged up into this changelog's single Unreleased
  section during the 0.4.0 bump rather than deleted, since they are real
  historical entries, not generator noise.
-->
- hoist compress::tool_output from trusty-agents ([#1959](https://github.com/bobmatnyc/trusty-tools/pull/1959)) ([#1968](https://github.com/bobmatnyc/trusty-tools/pull/1968)) ([`7cf93b9`](https://github.com/bobmatnyc/trusty-tools/commit/7cf93b9ab3918aff316238bdfe540a4053aa971d))
- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))

### Removed

- `agents::manifest::repair_stale_tmp` ([#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409)). It derived ONE scratch path from a target (`path.with_extension("tmp")`), which only holds while staging uses a single fixed name per target. With the per-process, per-attempt scratch names below there is no such thing as "the temp path for X", so the function was a silent no-op — and it made `tm repair deploy` report removing orphans it had left on disk, because `with_extension` strips only the last dot-segment and the round-trip never reconstructed the real name. Orphan cleanup is a `*.tmp` directory scan now, which needs no target-to-temp derivation.

### Documentation

- `ToolResult::is_error` / `is_fatal` no longer point their `Test:` doc
  comments at a test in the `cto-assistant` crate, which #3732 deletes. The
  assertions those pointers named are restored as unit tests beside the
  predicates they actually cover
  (`tool_result_is_error_distinguishes_variants`,
  `tool_result_is_fatal_only_for_non_recoverable`), so the crate's
  grandfathered row in `.test-pointer-allowlist.tsv` could be retired instead
  of left permanently unresolvable. No behaviour change.
- `agents::tier_audit`: corrected a misleading safety claim in the module doc and the `#4442` changelog fragment. Both previously said this module's untracked/user-owned handling "matches" `retract_framework_agents`, which never touches an untracked or user-owned file — implying `#4448`'s quarantine consumer could not touch an untracked file either. That parity holds for user-owned files only: `classify_tier_resident("qa", Untracked, {"qa"})` returns `ShadowsBundled`, pinned by the existing `classify_bundled_name_shadows` test. An untracked file on a bundled name is the intended target of a quarantine consumer, not an exclusion. Behavior is unchanged; this fixes the sentence a `#4448` implementer would otherwise read as a (false) data-loss safety proof ([#4442](https://github.com/bobmatnyc/trusty-tools/issues/4442), [#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448)).

## [0.3.0] — 2026-07-21

### Fixed

- `agents::builder::merge_frontmatter` now quotes an emitted scalar frontmatter value (`name`, `role`, `description`, `model`, `resource_tier`) whenever it needs quoting to stay valid YAML — e.g. a `description` containing a colon (closes #3556). Previously every scalar was emitted as a bare plain YAML scalar regardless of content, even though `split_frontmatter` had already stripped any quotes the source template used; a description like `Rust 2024 edition specialist: memory-safe systems` composed to an UNQUOTED line a strict YAML parser (`serde_yaml`, used by `trusty-agents`' `.md` agent loader) rejects with "mapping values are not allowed in this context", while trusty-mpm's own lenient reader tolerated it — so the bug was invisible to trusty-mpm's own tooling. Composition was invariant to source-side quoting, so re-provisioning alone could never have fixed an affected agent; this is a fix to the shared composer, not merely a re-deploy. `split_frontmatter` now symmetrically decodes the same escaping on parse so a compose → deploy → re-compose cycle round-trips verbatim.
- `needs_quoting` (the #3556 quote-on-emit check, above) also now catches a mid-string `" #"` sequence — a space followed by `#` starts a YAML comment ANYWHERE in a plain scalar, not just when `#` is the very first character (code-critic review of #3556's PR #3565, HIGH finding). A value like `Model context protocol #1 tool for delegating` previously composed to valid-but-truncated YAML — `validate_frontmatter` accepted it since truncated YAML is still syntactically valid, and the real consumer silently deserialized only `Model context protocol`, dropping everything after the `#` with no error anywhere. Same blind spot as the #3556 root cause, different trigger character. Also closes two smaller gaps found in the same review: the YAML core-schema null tokens (`null`/`Null`/`NULL`/`~`) now force quoting (they'd otherwise round-trip through `Option<String>` as `None` instead of the literal string), and an embedded newline in a scalar value now quotes AND escapes correctly (`escape_yaml_double_quoted`/`unescape_yaml_double_quoted` gained a `\n` case) rather than composing a physically multi-line frontmatter block.

### Added

- `agents::frontmatter::validate_frontmatter`: strict-parses a document's frontmatter block with `serde_yaml` — the same check a real consumer (e.g. `trusty-agents`' `.md` agent loader) applies, deliberately stricter than the crate's own lenient `parse_kv_line` grammar (#3556). `agents::deployer::deploy_agents_filtered` now calls it on every freshly composed agent before writing: a composition that fails strict validation is treated like a compose failure — logged loudly, recorded in `DeployResult::failed`, and skipped — so a malformed agent is caught at deploy time instead of silently landing in `.claude/agents/` and only failing at runtime.

## [0.2.3] — 2026-07-19

### Fixed

- `events::tests::publish_round_trips_through_subscribe` (and sibling tests `bus_is_singleton`, `seq_is_monotonic`) now carry `#[serial_test::serial]` — these three tests share the process-global `HarnessEvent` broadcast bus, so a concurrent workspace test run could deliver an unrelated event or interleave `publish()` sequence numbers between them, causing an intermittent assertion failure (closes #2961; same pattern as the #2271 `bm25_client` fix).

### Added

- new public `transport` module (`EventSource`, `MembershipProvider`, `SourceEvent`, `EventEnvelope`, `aggregate_live`): the harness-agnostic multi-client attach/fan-out transport extracted from `trusty-code::workstreams::sse` (issue #3299, DOC-48 §5.3.1/AC-7, epic #3292; enables trusty-agents epic #3052 adoption). Generic over an opaque group id and event payload — zero axum/tcode dependency, HTTP framing stays in each consumer. `trusty-code`'s `workstreams::sse` now implements `EventSource`/`MembershipProvider` over `crate::events`/`SharedWorkstreamStore` and delegates to `transport::aggregate_live`; behavior is unchanged (same test suite, ported unmodified). Additive only.
- new public `agents::builder_in_memory` module (`InMemorySources`, `build_in_memory_source_map`, `compose_agent_in_memory`): an in-memory counterpart to `agents::builder::compose_agent` that resolves `extends:` inheritance chains (including through BASE-* templates) against an embedded `name -> markdown` asset map instead of a filesystem `source_dir` (refs #2958, epic #2892 Slice E1 — the foundation for trusty-code embedding a curated tm agent roster as `include_str!` assets). Internally, `agents::builder::resolve` was generalised over a new `pub(crate)` `SourceLookup` trait so both the fs and in-memory paths share one chain-walk/cycle-detection/depth-limit implementation instead of forking it; the fs `compose_agent` API and its behavior are unchanged (verified by a byte-equivalence test comparing fs vs. in-memory composition of identical content). Additive only.
- `agents::builder::Frontmatter` (and the public `agents::metadata::AgentMetadata` it projects into) gains two fields — `max_tokens: Option<u32>` and `tools: Option<Vec<String>>` — making the shared frontmatter type a superset of tcode's TOML `AgentConfig` (refs #2897, epic #2892, Slice A). `max_tokens:` merges scalar child-wins across an `extends` chain, identically to `model:`. `tools:` merges by OVERRIDE (a child whose `tools:` key is present replaces the parent's list entirely) — deliberately distinct from `skills:`'s union/accumulate merge, so a restrictive leaf agent can narrow a permissive base's tool set. `tools:` is `Option`, not a bare `Vec`, so an omitted key (`None` → inherit the parent) stays distinguishable from an explicit `tools: []` (`Some(vec![])` → deny-all override) — mirrors tcode's `ToolsConfig.allowed: Option<Vec<String>>`. Purely additive and behavior-preserving for trusty-mpm: `tm`'s agents never set either key, so composed output for a tm-style agent stays byte-identical.

## [0.2.2] — 2026-07-17

### Added

- new public `agents` module (`agents::builder`, `agents::deployer`, `agents::manifest`, `agents::frontmatter`): the `extends:`-inheritance agent composer, the checksum + atomic-write ownership manifest, and the deploy pipeline extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892) (closes part of the #2892 extraction). `agent_manifest`'s error type is now a self-contained `ManifestError` (thiserror) so the shared crate carries no host-crate dependency. Additive only — no breaking changes to existing exports ([#2909](https://github.com/bobmatnyc/trusty-tools/pull/2909)) ([`bb947ea`](https://github.com/bobmatnyc/trusty-tools/commit/bb947ead9e220a37b8902b1190d261295c23538b))
- new public `skills` module (`skills::deployer`, `skills::manifest`, `skills::tiers`): the skills deploy/manifest/tiers machinery extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892, #2818). Additive only — no breaking changes to existing exports ([#2916](https://github.com/bobmatnyc/trusty-tools/pull/2916)) ([`488602d`](https://github.com/bobmatnyc/trusty-tools/commit/488602dfa5cc75916f33c66b555832ce310b0025))

## [0.2.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
