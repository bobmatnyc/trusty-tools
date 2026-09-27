# 0064. Instructional content is tracked, versioned, and deployed separately from code

- **Status:** Proposed
- **Date:** 2026-09-22
- **Scope:** Workspace-wide — bundled agents (`crates/trusty-agents-common/src/assets/agents/`),
  skills (`crates/trusty-mpm/src/assets/skills/`, `crates/trusty-code/src/assets/skills/`),
  PM instruction sections and output styles (`crates/trusty-mpm/src/assets/`),
  and the generated `tm-capabilities` catalog
- **Reversibility Cost:** Medium — the migration is a path move plus a
  version/changelog convention, not a rewrite; reverting means moving the
  tree back and dropping the runtime-fetch path, which nothing downstream
  depends on being absent
- **Decision Drivers:** owner ruling 2026-09-22 that instructional content
  needs semantic versioning, independent deployment, and its own changelog;
  the baseline rule that an agent or skill must work with any harness
  version; `scripts/detect-docs-only.sh`'s existing "Cargo-inert" boundary,
  which already refuses to treat `crates/*/src/**/*.md` as inert; the
  research brief in
  [docs/research/instructional-content-tracking.md](../research/instructional-content-tracking.md)
- **Supersedes / Superseded by:** Amends no prior ADR directly; narrows how
  ADR-0059 ("canonical agent behavior has generated host adapters") sources
  its canonical content

## Context

Every agent, skill, PM instruction section, and output style is a Markdown
file compiled into the `trusty-mpm` / `trusty-code` binaries via
`include_str!`, living under `crates/trusty-mpm/src/assets/` and
`crates/trusty-agents-common/src/assets/agents/`
(`docs/research/instructional-content-tracking.md` Part A.2). This makes
instructional content a first-class part of "crate source" for every gate
that keys on `crates/*/src/**`:

- `scripts/detect-docs-only.sh` explicitly classifies
  `crates/*/src/**/*.md` as NOT Cargo-inert — a one-line skill edit pays the
  full Rust build/clippy/test suite (Part A.12).
- `scripts/check_changelog_fragment.sh` requires a crate `changelog.d/`
  fragment for the same edit, alongside code changes it has nothing to do
  with (Part A.12).
- Staleness detection (`tm doctor`'s `skill_staleness` /
  `output_style_staleness` checks) defines "current" as "matches what THIS
  BINARY embeds" (Part A.7) — there is no notion of a content version
  independent of the binary that ships it.
- Only skills carry a `version:` frontmatter field today; none of the 43
  bundled agents do (Part A.8) — there is no versioning convention to build
  on for half the roster.
- A skill body names concrete `tm` subcommands and `Skill(skill=…)` calls
  with no capability probe and no advisory `requires:` block (Part A.11) —
  content is authored against one harness's exact surface, which is the
  direct opposite of "must work with any version."

The owner ruled (2026-09-22) that this coupling is wrong on two axes at
once: instructional content should have its own semantic version, changelog,
and release cadence, decoupled from any crate's SemVer or publish cycle; and
individual assets should degrade gracefully across harness versions rather
than assume one exact surface. A follow-up ruling made the CI-path
constraint explicit: a content-only change must trigger none of the SemVer
gate, the changelog-fragment gate, or a crate version bump — which requires
the content to live somewhere those path-scoped gates do not already reach,
not merely to be exempted by an added special case inside `crates/*/src/`.

## Decision

We will relocate instructional content — bundled agents, skills, PM
instruction sections, output styles, and the generated `tm-capabilities`
catalog — out of every crate's `src/` tree into a workspace-root `content/`
tree, versioned and changelogged independently of any crate:

1. **Location.** `content/{agents,skills,instructions,output-styles}/`
   at the workspace root. No crate's `Cargo.toml` version, and none of the
   crate-scoped CI gates (`detect-docs-only.sh`'s Cargo-inert check,
   `detect-version-bumps.sh`, `check_changelog_fragment.sh`,
   `check_line_cap.sh`'s `.rs`/`.swift` scan) reach a path outside
   `crates/`, so a content-only PR trips none of them once
   `detect-docs-only.sh` gains one explicit inert case for `content/**`.
2. **Versioning.** Every asset keeps (agents: gains) a `version:` semver
   frontmatter field. A `content/manifest.toml` — same `HarnessManifest`
   shape as `framework-manifest.toml` today — additionally declares one
   content-bundle version, which a check enforces is never lower than any
   member asset's version.
3. **Changelog.** `content/changelog.d/<issue>-<slug>.md` fragments,
   rolled into `content/CONTENT-CHANGELOG.md` — the same fragment mechanic
   crates already use, reused rather than reinvented.
4. **Distribution.** A path-filtered GitHub Actions job (safe here: not a
   required branch-protection check) packages `content/` on every
   `content/**`-touching push to `main` and publishes a GitHub Release
   tagged `content-vX.Y.Z`, independent of every crate's own release tags.
5. **Consumption.** `tm content update [--content-ref <tag>]` fetches and
   pins a content bundle into a local cache, recorded in
   `content-lock.toml`. `include_str!` of the in-repo `content/` tree stays
   compiled into the binary, but strictly as an OFFLINE BOOTSTRAP FALLBACK —
   preferred only when no cache exists and no network is reachable — never
   as the update channel.
6. **Compatibility.** No hard version gate anywhere in the load path. A
   `requires:` frontmatter block is advisory — `tm doctor` reports it, deploy
   never blocks on it. A skill or agent that names a harness-specific
   command states it as "probe, then degrade" (e.g. "run `tm --help`; if
   absent, fall back to …"), and a compatibility test matrix parses and
   dry-run-deploys every content asset against the oldest supported binary's
   manifest schema.
7. **Non-`tm` harnesses.** `trusty-code`, Codex, and Claude-Code-native
   consumers fetch the same published bundle through their own `content`
   command, or read the in-repo `content/` tree directly when co-located
   (as `trusty-code` already does today for `trusty-agents-common`'s
   agents).

## Consequences

**Easier:** a skill/agent/instruction edit becomes a docs-rung (rung 1) PR —
no Rust build, no changelog-fragment gate false positive, no crate version
bump; instructional content can ship on its own cadence, faster than a
`trusty-tools` binary release, without becoming a fake crate patch release;
`tm doctor` can report content-version-vs-binary-version as two independent
facts instead of conflating "stale" with "binary rebuilt."

**Harder:** every `include_str!` call site across `trusty-mpm`,
`trusty-agents-common`, and `trusty-code` moves and must be re-pointed in
one coordinated PR (the migration's step 1); `extends:`-chain resolution
(`agents::builder::SourceLookup`) must keep resolving within the new
single `content/agents/` directory — no code change needed there, but the
directory move must not split the roster across two locations; the
`pm-prompt-*.md` goldens, `check_capabilities.sh`'s drift diff, and
`check_context_budget.sh`'s scanned-path list all need one mechanical
update to the new paths in the same PR that moves the tree, or they go red
for a reason unrelated to their actual purpose.

**Neutral / follow-up:** `content/manifest.toml`, `content-lock.toml`, the
`tm content` subcommand family, and the compatibility test matrix are new
surfaces with their own tests to write — tracked as the second and third
PR-sized steps in the research brief, not part of this ADR's first step.
The `trusty-code` skill-refs mirror test
(`referenced_copies_match_trusty_mpm_skills`) either becomes redundant (both
sides now read the one `content/` tree) or needs updating to the new path —
resolved when `trusty-code`'s own asset tree is folded into `content/` in a
later step, deliberately out of scope for step 1.

## Related Decisions

Vetted against prior decisions on 2026-09-22:

- **ADR-0059 (Canonical agent behavior has generated host adapters):**
  Extends — that ADR establishes ONE canonical agent-behavior source with
  generated per-host adapters; this ADR relocates where that canonical
  source physically lives (`content/` instead of `crates/*/src/`) without
  changing the generation relationship. Consistent.
- **ADR-0058 (Trusty Code is an independent, product-owned harness):**
  Consistent — `trusty-code` keeps its own asset subset and its own deploy
  path; this ADR only changes where the shared upstream content lives, not
  `trusty-code`'s product boundary or its `.trusty-code/` state root.
- **ADR-0025 (Collapse the agent and skill tier hierarchies — one deploy
  target, many declared sources, precedence resolved at deploy time):**
  Consistent — that ADR fixed WHERE deployed copies land and in what
  precedence, via `manifest.toml` (of which `framework-manifest.toml` is one
  tier); this ADR only relocates where the framework tier's SOURCE content
  is authored and versioned before deploy. Deploy targets, precedence, and
  the manifest format are unchanged.
- No other prior ADR governs asset location, versioning, or CI-gate scoping
  for instructional content specifically.
