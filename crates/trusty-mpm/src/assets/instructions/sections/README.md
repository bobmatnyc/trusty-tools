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

Nothing constrains order by section any more. A rule used to require that the
four "floor" sections were the contiguous tail; it was deleted with the floor.

## Customization tiers — `core` is the only protected section

Every section declares a `customization_tier`:

- `fixed` — **`core`, and only `core`.** A `CORE` marker in a project's
  `CLAUDE.md` is declined and logged; the bundled core section stays in force.
- `project` — **every other section**: `identity`, `memory`, `search`,
  `workflow`, `agent-delegation`, `enforcement`, `non-overridable-rules`,
  `framework-guaranteed-conventions`. A project may replace any of them.

`InstructionPackage::validate` enforces that as an **iff**: `core` must be
`fixed` and nothing else may be. Both directions are red, because retiering
`core` to `project` would leave nothing protected at all, and marking any other
section `fixed` would quietly reinstate the floor described below.

### Why there is no framework floor

There used to be one: four `fixed`-tier sections nothing could override, a
`SectionId::is_floor()` predicate, a rule that nothing overridable could follow
them, and a CI guard (`check_instruction_floor.sh`) pinning their bytes with
sha256 digests. All of it is gone.

The owner's reasoning: **a project owns its own `CLAUDE.md`.** Claude Code
memory-loads that file natively, and nothing in this system can stop a project
from writing whatever it likes there. A floor therefore bought the *appearance*
of a control, not a control — while costing real complexity (a byte-pin, a
regeneration ritual, a CI job, and a validation rule) and inviting the belief
that framework text was guaranteed when it was not.

**The accepted consequence, chosen rather than overlooked:** content that used
to be non-overridable now is not. A project can replace the Prohibitions and
Circuit Breakers tables (`enforcement`), the commit/PR attribution footer and
the documentation conventions (`framework-guaranteed-conventions`), the Trusty
Tool Priority mandate (`non-overridable-rules`), the PM's identity and
direct-action budget (`identity`), and `never turn red green by deleting
coverage` (`workflow`). Overriding one section still takes only that section —
a `WORKFLOW` block does not disturb `enforcement` — but nothing outside `core`
is protected from a project that explicitly asks to replace it.

Do not "fix" this by promoting content into `core.md`. That was considered and
rejected: relocating content to preserve protection defeats the point of
removing the mechanism, and would turn `core.md` into a dumping ground.

## How a project overrides a section

In the project's root `CLAUDE.md`:

```
<!-- TRUSTY-MPM: WORKFLOW START v=1 -->
…replacement text…
<!-- TRUSTY-MPM: WORKFLOW END -->
```

The token is the section id, uppercased. Every token is accepted except `CORE`,
which is always declined with a logged warning.

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

This is also the fact the floor removal rests on: the project's own file reaches
the model whatever this code does.

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

## Retired — do not bring these back

| Retired | Why it must not return |
|---|---|
| The five `.trusty-mpm/` override files (`PM_INSTRUCTIONS_DEPLOYED.md`, `AGENT_DELEGATION.md`, `WORKFLOW.md`, `MEMORY.md`, `INSTRUCTIONS.md`) | Whole-document granularity let a project silently delete framework content while believing it had added to it. `CLAUDE.md` named sections replace them at section granularity (#4286). A leftover file fails `tm doctor`'s `legacy_overrides` check. |
| The framework floor: `is_floor()`, `FloorNotFixed`, `OverridableAfterFloor`, `validate_floor_is_last`, `check_instruction_floor.sh`, `instruction_floor.sha256`, `.github/workflows/instruction-floor-guard.yml` | It was the appearance of a control (#4286). Reinstating any part means re-adopting a guarantee the system cannot actually make. |
| `BASE_PM.md` | The monolithic floor asset, deleted by #4183 and decomposed into the sections here. There is nothing left to fold in and nothing to restore. |
| `assets/instructions/CLAUDE.md` | A dead stub, registered but never read back. Removed by #3374. |

`base_pm()` in `instruction_pipeline.rs` and the former
`# BASE_PM Framework Floor` heading are historical labels. The heading now reads
`# Framework Instructions`; the function name survives and is misleading — see
the findings below.

## Findings

Written against the actual code, these do not explain cleanly. Recorded rather
than smoothed over.

1. **`base_pm()` is now a misnomer twice over.** It refers to a file that does
   not exist, and it hardcodes four section ids in one order to reconstitute a
   "floor" that no longer exists as a concept. It is still live — the
   roster-absent string assembly appends it — so it is a real function whose
   name describes nothing true. Renaming it is mechanical and was left out of
   #4286 to keep that change reviewable.

3. **The `project-addendum` generator is declared but unfeedable.** Its only
   production input was the retired `.trusty-mpm/INSTRUCTIONS.md`. The block
   stays declared in the manifest (marked `optional`, so it emits nothing) and
   the composer always passes `None`.

### Retired findings

- *"`never turn red green by deleting coverage` is not floor-protected."*
  Resolved by the ruling, not by a fix: **nothing** outside `core` is protected
  now, so this stopped being an anomaly and became the documented model. See
  "Why there is no framework floor" above.
