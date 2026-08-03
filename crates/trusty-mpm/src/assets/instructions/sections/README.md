# PM instruction sections

The PM system prompt is assembled from the markdown files in this directory.
This file explains how, well enough to change the system safely.

Spec of record: `docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md`.
Its §9 carries the 2026-08-03 rulings this directory now reflects.

Code is referenced by **file and symbol name**, never by line number. Line
numbers drift; names are greppable.

## The shape in one paragraph

Nine markdown files here are the authored prose. A JSON manifest one level up,
`pm-instruction-package.json`, declares which sections exist, what tier each is,
and the ordered stream of blocks that fill them. At session launch a composer
reads the manifest, resolves each block to text, folds in two pieces of
host-computed content, applies any project overrides, and joins the result. The
composed string is written to a temp file and passed to `claude` via
`--append-system-prompt-file`.

## Where things live

| Thing | Location |
|---|---|
| Section prose | this directory, one `.md` per section |
| Section/block declarations | `assets/instructions/pm-instruction-package.json` |
| Schema for that manifest | `assets/instructions/instruction-package.schema.json` |
| Path → embedded source table | `instruction_pipeline.rs`, `SECTION_SOURCES` |
| Manifest parsing / validation | `instruction_package.rs`, `InstructionPackage` |
| Composition | `bundled_pm_package.rs`, `compose_bundled_fallback_with_overrides` |
| Entry point | `instruction_overrides.rs`, `resolve_pm_prompt` |
| Project override reader | `claude_md_sections.rs`, `scan_project` |

Section files are embedded with `include_str!`, so a missing file is a compile
error, not a launch-time surprise. Adding a section means: add the `.md` here,
add its `include_str!` constant and `SECTION_SOURCES` entry in
`instruction_pipeline.rs`, add a `SectionId` variant in `instruction_package.rs`,
and declare the section plus at least one block in the manifest.

## Ordering

The manifest's `blocks` array alone decides emission order. The `sections` array
declares metadata, not sequence. Each block names a `join_before` (`rule` for a
`---` separator, `blank` for a paragraph break), which is what makes the output
reproducible without any code knowing the running order.

## Customization tiers

Every section declares a `customization_tier`:

- `project` — a project may replace it: `core`, `memory`, `search`, `workflow`,
  `agent-delegation`.
- `fixed` — nothing can replace it: `identity`, `enforcement`,
  `non-overridable-rules`, `framework-guaranteed-conventions`.

The four `fixed` sections are the framework floor. `enforcement` (the
Prohibitions and Circuit Breakers tables) is `fixed` because of #4573: it used
to live inside `core`, and a three-line `CORE` override deleted both tables while
the floor went on asserting a table that was no longer in the prompt.

Who decides is worth stating precisely, because it is the property that makes
the floor safe: `claude_md_sections.rs` holds no list of overridable sections. It
asks `CustomizationTier::permits` for the tier the shipped manifest declares. A
second list would be a second source of truth, and the floor would become
overridable the first time the two disagreed.

## How a project overrides a section

In the project's root `CLAUDE.md`:

```
<!-- TRUSTY-MPM: WORKFLOW START v=1 -->
…replacement text…
<!-- TRUSTY-MPM: WORKFLOW END -->
```

The token is the section id, uppercased. Accepted: `CORE`, `MEMORY`, `SEARCH`,
`WORKFLOW`, `AGENT-DELEGATION`. Always declined with a logged warning:
`IDENTITY`, `ENFORCEMENT`, `NON-OVERRIDABLE-RULES`,
`FRAMEWORK-GUARANTEED-CONVENTIONS`.

An override replaces the section's authored blocks. It cannot touch that
section's **generated** blocks — so an `AGENT-DELEGATION` override rewrites the
routing doctrine and the live agent roster still follows it.

Everything fails toward more framework instruction, never less. An unclosed
marker, an unknown token, an unknown version, an empty body, or an override that
invalidates the package all resolve to "keep the bundled section". A
customization mistake must never be able to delete the PM's instructions.

## CLAUDE.md is read twice, by two different readers

This trips people up, so state it plainly:

- **Claude Code** memory-loads the project's `CLAUDE.md` natively, on its own.
- **The composer** deliberately does NOT put `CLAUDE.md` prose into the system
  prompt. See `instruction_pipeline.rs` — CLAUDE.md is excluded from
  `resolve_pm_prompt`'s output.

The composer reads `CLAUDE.md` for exactly one purpose: extracting marked
override blocks. Prose outside the markers is ignored by the composer, because
Claude Code has already loaded it. Including it would double-load it.

## Dynamic content

Two blocks are computed at launch, not authored:

- **`agent-roster`** — the live deployed-agent inventory, built by
  `delegation_authority.rs` (`deployed_roster_section`), which unions the project
  tier, `$CLAUDE_CONFIG_DIR/agents`, and `~/.claude/agents`. Appended inside the
  `agent-delegation` section.
- **`stack-profile`** — the project's detected stack, from `stack_profile.rs`.
  Optional; emits a neutral detect-first block when nothing is detected.

The roster also selects the composer. With a roster, the packaged composer runs.
With none — no agent deployed in any tier — composition degrades to
`assemble_sections` in `instruction_overrides.rs`, a string assembly where only
`WORKFLOW`, `MEMORY` and `AGENT-DELEGATION` are independently addressable. Any
other named override is reported as unapplied there rather than dropped silently.

## The pinned floor

`scripts/check_instruction_floor.sh` pins the floor byte-exactly. Its digests
live in `scripts/instruction_floor.sha256` and cover:

1. every `.md` sourced by a `fixed`-tier section,
2. a canonical projection of the manifest's fixed sections and their blocks, so
   retiering a floor section to `project` or repointing its file is caught even
   when no prose changed,
3. the guard's own workflow file.

Any byte difference fails the build — including an inversion, a truncation, or
wrapping the floor in an HTML comment, all of which a substring check waves
through. That is why it is a digest and not a `grep`.

Changing the floor is a **two-part commit**: the edit, plus
`bash scripts/check_instruction_floor.sh --update` and the regenerated digest
file. Regenerating without a reviewed floor change defeats the guard. Report
which digests moved.

## Retired — do not bring these back

| Retired | Why it must not return |
|---|---|
| The five `.trusty-mpm/` override files (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`, `MEMORY.md`, `INSTRUCTIONS.md`) | Whole-document granularity let a project silently delete owner-required framework content. `CLAUDE.md` named sections replace them at section granularity (#4286). A leftover file now fails `tm doctor`'s `legacy_overrides` check. |
| `BASE_PM.md` | The monolithic floor asset. Deleted by #4183 and decomposed into the four `fixed` sections. `check_instruction_floor.sh` hard-fails if the file reappears — two sources for the floor is the drift #3374 removed. |
| `assets/instructions/CLAUDE.md` | A dead stub, registered but never read back. Also hard-failed by the floor guard. |

`base_pm()` in `instruction_pipeline.rs` and the `# BASE_PM Framework Floor`
heading in `identity.md` are surviving **labels**, not a surviving file. The
function reconstitutes the four `fixed` sections into one string.

## Findings this README surfaced

Writing this against the actual code turned up three things that do not explain
cleanly. They are recorded rather than smoothed over.

1. **Two writers target one path.** `bundle_all.rs` writes a 4-line stub to
   `instructions/INSTRUCTIONS.md`, and `instruction_pipeline.rs`'s
   `install_system_prompt` writes the *full composed prompt* to the same
   `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`. Whichever runs last
   wins. Neither is read by the launch path, which composes in memory. This is
   the bundled `assets/instructions/INSTRUCTIONS.md` retirement, still open.

2. **`base_pm()` has no single honest name.** It is called "the BASE_PM floor"
   but BASE_PM does not exist; it is "the fixed-tier sections" but it hardcodes
   four ids in one order rather than deriving them from the manifest's tier
   declaration. A section retiered to `fixed` would be pinned by the digest guard
   and still not appear in `base_pm()`'s output.

3. **The `project-addendum` generator is declared but unfeedable.** Its only
   production input was the retired `.trusty-mpm/INSTRUCTIONS.md`. The block
   stays declared in the manifest (marked `optional`, so it emits nothing) and
   the composer now always passes `None`. It is a loaded asset whose purpose
   cannot currently be named — exactly the smell this README was meant to catch.
