# tm-ticketing — labels

## Labels

Three separable families, on issues. The rule that they are applied at all is
"The Labels, Project, Milestone Standard" in `SKILL.md`; the exact command form lives in
the agent asset ("Label at Creation"), so a delegation brief never needs to
spell either out.

| Family | Cardinality | Content |
|---|---|---|
| Type | exactly one | `bug`, `enhancement`, `refactor`, `chore`, `documentation`, `epic` |
| Owning component | one or more | The project's own unit of ownership that the defect lives in — a Cargo crate, an npm/pnpm workspace package, a `pyproject.toml` distribution, a Go module, a deployable service directory |
| Priority | optional | `P0`–`P3`, **only** when the issue text itself asserts severity. A guessed priority is noise |

🔴 **There is no fourth family for where a finding came from.** Which session,
harness, agent, or tool surfaced an issue is never a labeling input, under any
name — not "provenance", not "umbrella", not "dogfooding". `trusty-mpm` is an
owning-component label like any other: it applies only when the code at fault
sits under `crates/trusty-mpm/` (or the tm CLI's own release tooling), never
because the session that filed the issue happened to run under tm. When no
component label fits, apply none — that decision is final, not a trigger to
reach for `trusty-mpm`.

Then post a `no-component-label: <reason>` comment on the issue in the same
dispatch, exactly as an unset milestone takes a `no-milestone: <reason>` one.
That comment is the only thing that makes an absent component label legitimate:
`tm issue audit` reads it and prints `component label  SKIP  <reason>` instead
of FAIL (#7198). 🔴 **The prefix is parsed literally — keep
`no-component-label:` byte-for-byte** — while the reason is written in the
project's own vocabulary: "no <stack unit> owns the path". The `website/`,
`scripts/` and CI-only shape is what it is for.

🔴 **The component axis is stack-neutral.** The unit is whatever the project
builds and owns in: a Cargo crate here, an npm/pnpm workspace package in a
TypeScript monorepo, a `pyproject.toml` distribution, a Go module, a Gradle
subproject, a deployable service directory. Take it from the project's
`TICKETING.md` when it names one, else from the manifests actually present in
the tree. A generated `TICKETING.md` states the taxonomy its generator derived
for THAT repository, and a waiver reading "no Cargo crate owns this" in a
project with no Cargo is a defect.

🔴 **Seed the harness's own labels on first use in a repository.** `tm issue
seed-labels` creates the four `status:*` lifecycle labels, `trusty-mpm`, and
`ws/<session>`. It is idempotent and never rewrites a label that already
exists, so run it rather than checking first. Re-run it whenever a `gh issue
edit --add-label` or `gh issue create --label` fails on an unknown label, then
retry the original command once.

🔴 **Never invent a label the repository does not carry.** Check `gh label list`
before using one; create a genuinely missing label rather than dropping the
family or substituting an approximation.

🟡 **The standard is configurable — read it, do not assume it.** `tm issue
standard` prints the ticketing standard in effect: the component labels, the
lifecycle labels, the default assignee, and whether a claim comment and a
closing note are expected. Those values come from the `agents.ticketing` block
in `~/.trusty-tools/trusty-mpm/config.yaml`, so a project can add a component
label, restyle one, name a different assignee, or point at its own
`issue-state.yaml` (#6918). `tm issue seed-config` WRITES that block into
`config.yaml` — it creates the file, or appends the block to an existing one,
leaving every prior byte and an existing `agents.ticketing` untouched (#7067).
One case it refuses: a file that already declares `agents:` without
`ticketing:`, since a second top-level `agents:` key would make the whole file
unparseable and cost the operator every other setting in it. There it writes
nothing, prints the block, and **exits nonzero** — paste the block under the
existing `agents:` key by hand, then re-run to confirm.
Two things the block cannot change, and the command prints both: the PR
issue-link keyword stays `Refs #N` (a one-off `Closes` is the deliberate
`tm pr open --closes` flag), and `trusty-mpm` stays a component label, never a
lifecycle one. A block that tries either is refused at load with the field
named.
