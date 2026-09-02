# Agent and Skill Tier Hygiene

> Where bundled agents and skills are allowed to live, what tm checks about that
> at session launch and in `tm doctor`, and which of those findings tm will
> repair for you. Owner ruling 2026-09-02, issue
> [#6649](https://github.com/bobmatnyc/trusty-tools/issues/6649).

## The tier model in one table

| Asset | Canonical tier | Project tier | Which one loads |
|---|---|---|---|
| Agents | `$CLAUDE_CONFIG_DIR/agents/` | `<project>/.claude/agents/` | Claude Code resolves **project first**, so a project copy SHADOWS the canonical one (#4408) |
| Skills | `$CLAUDE_CONFIG_DIR/skills/` | `<project>/.claude/skills/` | Claude Code resolves `enterprise > personal > project`, so the **user** copy wins |

Bundled assets belong to the canonical tier only — agents since
[#4409](https://github.com/bobmatnyc/trusty-tools/issues/4409), skills since
[#6586](https://github.com/bobmatnyc/trusty-tools/issues/6586). A project's OWN
agents and skills are legitimate and are never touched: every check below keys on
the bundled roster, so a name the roster does not carry produces no finding
anywhere.

## The three checks

Each runs at session launch AND as a `tm doctor` row. At launch the finding is one
line; a clean project produces no line at all.

### 1. Agents quarantined

A project-tier agent file whose name the bundled roster carries, which tm can
prove it did not author, is MOVED aside at every launch
([#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448)) — renamed to an
inert `<name>.md.disabled` with a verified backup under
`.trusty-mpm/agent-quarantine/`. Nothing is deleted.

- Launch line: `agents quarantined N (names), M could not be moved`
- Doctor row: `asset_tier`
- Repair: `tm doctor --fix-agents` (see below)

### 2. Skills stray

A bundled skill still sitting at a project's `.claude/skills/`. No deploy writes
these any more and no deploy refreshes them, so each freezes at the text that
shipped the day it landed while the user-tier copy moves on.

- Launch line: `skills stray N (names) at the project tier <path>`
- Doctor row: `skill_project_tier`
- Repair: `tm doctor --fix-skills` (previews) / `--yes` (applies)

### 3. Duplicates

One asset name claimed by TWO entries inside ONE tier: `qa.md` beside a `qa/`
directory, or `QA.md` beside `qa.md`. Only one ever loads, which one is loader
order, and on macOS the filesystem is case-insensitive so the two names are one
file. The two checks above both compare one directory against another and are
structurally unable to see this.

- Launch line: `duplicates N (names)`
- Doctor row: `asset_duplicates`
- Repair: **none, by design.** tm cannot know which entry you meant to keep, and
  both may be yours. Delete or rename one.

## `tm doctor --fix-agents`

The agent mirror of `--fix-skills`. It PREVIEWS by default; `--yes` applies.

```bash
tm doctor --fix-agents          # preview — writes nothing at all
tm doctor --fix-agents --yes    # apply
```

It removes a project-tier agent file only when ALL of these hold, and reports a
refusal with the reason otherwise:

1. The entry is a plain `.md` file. A bundled-named **directory** is the shape an
   operator creates by hand; tm cannot tell what is in it and never removes it.
2. That tier's own `.trusty-mpm-manifest.json` records the file. An untracked
   file may be your own agent under a bundled name.
3. The ledger records the origin as tm's, not yours — the same
   `Origin::is_framework_owned` predicate `retract_framework_agents` uses.
4. The bytes still match the recorded checksum. A mismatch is a hand edit, and
   your edit is not tm's to delete.

Every removal is copied to `~/.trusty-mpm/backup-doctor-remediation-<ts>/project-agents/`
first and confirmed by re-reading disk. The sweep refuses a project tier that is a
symlink, one it cannot list, and one that resolves onto the canonical deploy dir
or your `~/.claude/agents` — removing the roster from the tier it is supposed to
live in would run #4409 backwards.

`tm doctor --fix` does NOT run this sweep. `--fix` still never deletes.

## Fail-open is a finding, not silence

A tier that cannot be listed and a roster that cannot be built each produce an
`UNDETERMINED` line at launch and an `Unknown` doctor status — never a clean bill
of health for a question that was never answered ([ADR-0045]). The `--fix-agents`
sweep answers the same way: a corrupt ledger or an empty roster is a tier-wide
refusal that removes nothing and says why.

## Where the code is

| Concern | Path |
|---|---|
| Launch lines | `crates/trusty-mpm/src/core/session_launch/asset_notices.rs` |
| Duplicate detector | `crates/trusty-mpm/src/core/asset_duplicates.rs` |
| Agent sweep | `crates/trusty-mpm/src/core/project_tier_agent_strays.rs` |
| Skill sweep | `crates/trusty-mpm/src/core/project_tier_strays.rs` |
| Doctor rows | `crates/trusty-mpm/src/daemon/doctor_asset_tier.rs`, `doctor_asset_duplicates.rs`, `doctor_skill_project_tier.rs` |

[ADR-0045]: ../adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md
