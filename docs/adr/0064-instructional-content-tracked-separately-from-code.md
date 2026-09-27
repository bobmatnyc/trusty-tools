# 0064. Instructional content is tracked, versioned, and deployed separately from code

- **Status:** Accepted
- **Accepted:** 2026-09-22 (owner ruling, [epic #8378](https://github.com/bobmatnyc/trusty-tools/issues/8378)); scope and scheduling confirmed 2026-09-23 10:55Z ([#8387](https://github.com/bobmatnyc/trusty-tools/issues/8387)) — decisions 3 and 4 (moving `sm_instructions/`, `harness_understanding/`, and the 43 shared agents; `hooks/` and `trusty-code`'s 12 local agents stay put until PHASE_4, [#8390](https://github.com/bobmatnyc/trusty-tools/issues/8390)) confirmed within this ADR's content classes (four at the time; a fifth, `product-prompts`, was added by the 2026-09-27 ruling below), no ADR change needed; PHASE_1 joins the merge queue after 1.7.1 ships.
- **Amended:** 2026-09-27 (owner ruling) — decision 5's compiled
  offline-bootstrap fallback is dropped; content distribution is
  runtime-only. See "Superseded 2026-09-27" under Decision item 5.
- **Amended:** 2026-09-27 (owner and supervisor ruling) — the runtime
  resolver ships before any file move; the move and the removal of every
  embed land together, split into five per-crate PRs in publish order (see
  the 2026-09-27 split-per-crate amendment below); scope adds three more
  embeds (trusty-agents' `rust-embed` agents and workflows, trusty-review's
  prompt templates, trusty-mpm's `bundle.rs` embedded docs); a fifth
  content class, "product prompts," covers trusty-review's templates; the
  sha256/pin integrity check has one implementation, in trusty-common,
  called by trusty-installer. See "Sequencing" under Decision item 5 and
  the updated Decision item 1 and Scope below.
- **Amended:** 2026-09-27 (owner ruling) — decision 5's move-and-embed-drop
  splits into five PRs, one per crate, merging in publish order:
  trusty-agents-common, then trusty-mpm, then trusty-code and
  trusty-agents, then trusty-review. Each PR moves that crate's content
  into `content/**` and removes that crate's embeds in the same PR, so no
  published crate ever references a file outside its own package. See
  "Sequencing" under Decision item 5.
- **Date:** 2026-09-22
- **Scope:** Workspace-wide — bundled agents
  (`crates/trusty-agents-common/src/assets/agents/`), skills
  (`crates/trusty-mpm/src/assets/skills/`), PM instruction sections, output
  styles (`crates/trusty-mpm/src/assets/`), and the generated
  `tm-capabilities` catalog. `sm_instructions/` and `harness_understanding/`
  move to `content/instructions/`, per the 2026-09-23 ruling on #8387.
  trusty-code's own 12 agents and skills stay in
  `crates/trusty-code/src/assets/` until PHASE_4 (#8390). trusty-code's
  re-export of trusty-agents-common's agent consts switches to `content/`
  in PHASE_3. A supervisor ruling (2026-09-27) holds that the #8389
  non-goal covers only trusty-code's own asset tree, not that re-export.
  Scope also adds three more embeds (owner ruling 2026-09-27): trusty-agents'
  `rust-embed` agents and workflows
  (`crates/trusty-agents/src/agents/bundled/mod.rs`), moving into
  `content/agents/trusty-agents/` — namespaced apart from the
  trusty-agents-common roster already at `content/agents/`, so the two
  rosters do not collide; trusty-review's prompt templates
  (`crates/trusty-review/src/pipeline/prompt_templates.rs`), covered by the
  new "product prompts" class at `content/product-prompts/trusty-review/`;
  and trusty-mpm's `bundle.rs` embedded docs, covered by the "instructions"
  class
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

1. **Location.** `content/{agents,skills,instructions,output-styles,
   product-prompts}/` at the workspace root — five content classes (owner
   ruling 2026-09-27 adds `product-prompts`, covering a role-specific
   reviewer system prompt such as trusty-review's, which fits none of the
   original four). A JSON workflow definition that a bundled agent depends
   on, such as trusty-agents' pipeline defs, is not a class of its own; it
   lives beside the agent files that use it, under `agents/`. No crate's
   `Cargo.toml` version, and none of the crate-scoped CI gates
   (`detect-docs-only.sh`'s Cargo-inert check, `detect-version-bumps.sh`,
   `check_changelog_fragment.sh`, `check_line_cap.sh`'s `.rs`/`.swift` scan)
   reach a path outside `crates/`, so a content-only PR trips none of them
   once `detect-docs-only.sh` gains one explicit inert case for
   `content/**`. An add or modify under `content/**` is inert for CI even
   while it is still compiled in via `include_str!`; a delete or rename
   there stays code, because it changes a compile-time path (owner ruling
   2026-09-27).
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
   This job exists before `content/` does: its first run is a manual
   dispatch that packages a manifest of today's scattered in-crate asset
   directories, so a seed `content-v0.1.0` tag exists for the resolver
   below to integration-test against, ahead of any file move (owner ruling
   2026-09-27; see "Sequencing").
5. ~~**Consumption.** `tm content update [--content-ref <tag>]` fetches and
   pins a content bundle into a local cache, recorded in
   `content-lock.toml`. `include_str!` of the in-repo `content/` tree stays
   compiled into the binary, but strictly as an OFFLINE BOOTSTRAP FALLBACK —
   preferred only when no cache exists and no network is reachable — never
   as the update channel.~~
   **Superseded 2026-09-27 (owner ruling):** drop the compiled fallback —
   runtime only. Nothing about agents, skills, instructions, output styles,
   or product prompts is compiled into any binary: no `include_str!` and no
   `RustEmbed` of content anywhere in the load path. On first run, `tm`
   fetches the pinned content release (`content-vX.Y.Z`) into
   `~/.trusty-mpm/content` and records it in `content-lock.toml`. With no
   cache and no network, `tm` says so and names the command to run. Content
   changes never touch a crate version or trigger a Rust build. Rationale:
   `cargo install` copies binaries only — Cargo has no install model for
   data files — and a published crate cannot `include_str!` files outside
   its own directory.

   **Sequencing (amended 2026-09-27, owner and supervisor ruling; split per
   crate 2026-09-27, owner ruling).** PHASE_3 (the runtime resolver) ships
   before PHASE_1 (the file move). PHASE_1 then moves every embedded asset
   out of `crates/*/src/` and removes every embed macro that currently
   reads it, split into five PRs, one per crate, merging in publish order:
   trusty-agents-common, then trusty-mpm, then trusty-code and
   trusty-agents, then trusty-review. Each crate's move and its embed
   removal land together in that crate's PR, so no published crate ever
   references a file outside its own package — not in a first PR that
   moves files while embeds stay compiled in and a second PR that drops
   them. Nothing in this sequence freezes any crate's `cargo publish`; the
   one ordering constraint runs the other way, on the content side — a
   `content-v*` release must exist before the next crate publish that
   depends on the resolver, so PHASE_3's own tests have a real tag to
   verify against. This supersedes the earlier phrasing under this item,
   which deferred the embed removals to a later phase while PHASE_1 moved
   only the files.

   **PHASE_3 acceptance criteria (owner ruling 2026-09-27).** PHASE_3 ships
   only once all four hold:

   i. **Integrity.** `content-lock.toml` pins the installed release's tag
      and a sha256 of the bundle. `tm` refuses to run against an installed
      bundle whose sha256 does not match the pinned value. This is one
      integrity implementation, not two: the sha256/pin verification moves
      out of trusty-installer's `download/pinned.rs` into trusty-common,
      and trusty-installer calls the trusty-common implementation for its
      own pinned-tool downloads rather than keeping a parallel copy. If
      that move forces a trusty-installer publish, it joins the same
      crate-republish order the rest of this ADR's move already forces.
   ii. **Offline install.** `tm content install --from <bundle.tar.gz>`
       installs a content bundle from a local file, with no network
       reachable.
   iii. **Dev override.** Run from inside the `trusty-tools` checkout, `tm`
        reads `content/` directly, bypassing the installed cache.
   iv. **Schema major version.** Content frontmatter carries a schema major
       version. `tm` refuses to load a bundle whose schema major version is
       newer than the one it supports, and states why.

   The content version stays advisory (decision 6 is otherwise unchanged):
   none of the four conditions above adds a hard version gate. Hooks,
   `pm-guard`, and the deployer stay in code — PHASE_3 relocates
   instructional content only.
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

**Harder:** PHASE_1 moves every file `include_str!` or `RustEmbed` currently
embeds — across `trusty-mpm`, `trusty-agents-common`, `trusty-agents`,
`trusty-review`, and `trusty-code` — into the new `content/` tree and
removes the embed macro at every one of those call sites, split into five
per-crate PRs merging in publish order (see "Sequencing" under Decision
item 5); with no cache and no network, `tm` names the command to run, and
content changes never touch a crate version. Two of the added embeds are
more than a path swap:
trusty-review's `prompt_templates.rs` and trusty-mpm's `bundle.rs` currently
expose `pub const &'static str` values, and their resolver-backed
replacements are fallible — every call site that reads one now handles a
`Result`, a type-signature change, not a mechanical rename. `extends:`-chain
resolution (`agents::builder::SourceLookup`) must keep resolving within the
new single `content/agents/` directory — no code change needed there, but
the directory move must not split the roster across two locations; the
`pm-prompt-*.md` goldens, `check_capabilities.sh`'s drift diff, and
`check_context_budget.sh`'s scanned-path list all need one mechanical
update to the new paths, landing in whichever crate's PR moves the path it
covers, or they go red for a reason unrelated to their actual purpose.

**Neutral / follow-up:** `content/manifest.toml`, `content-lock.toml`, the
`tm content` subcommand family, and the compatibility test matrix are new
surfaces with their own tests to write — tracked as the second and third
PR-sized steps in the research brief, not part of this ADR's first step.
The `trusty-code` skill-refs mirror test
(`referenced_copies_match_trusty_mpm_skills`) either becomes redundant (both
sides now read the one `content/` tree) or needs updating to the new path —
resolved when `trusty-code`'s own asset tree is folded into `content/` in a
later step, deliberately out of scope for step 1.

**Decided (2026-09-27, owner ruling):** PHASE_1's move-and-embed-drop splits
into five PRs, one per crate, merging in publish order:
trusty-agents-common, then trusty-mpm, then trusty-code and trusty-agents,
then trusty-review. Each PR moves that crate's content into `content/**`
and removes that crate's embeds in the same PR, so no published crate ever
references a file outside its own package.

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
