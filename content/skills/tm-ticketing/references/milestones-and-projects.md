# tm-ticketing — milestone and project

## Choosing the Milestone and the Project (issues)

The standard that an issue carries both is "The Labels, Project, Milestone
Standard" in `SKILL.md`. This section is how the two values are chosen and set —
and the relationships that go with them, which are the issue side's alone:
parent/child and blocked-by, set with the same `gh` call that files the issue
("Relationships (native, never prose)", `references/relationships.md`).

`tm issue standard` is the source of truth for all three. It prints
`milestone_required`, `project_required`, the configured `default_project`, and
the live lists of open milestones and open projects. Read it before the first
filing in a repository; never hand-type a title it did not print.

```bash
gh issue create --title "…" --body "…" \
  --milestone "Backlog · mpm/core" --project "trusty-mpm" \
  --label bug --label trusty-mpm
gh issue edit 7067 --milestone "mpm 1.4"
gh project item-add 25 --owner bobmatnyc --url https://github.com/…/issues/7067
```

Installed `gh` is 2.96; `--milestone` and `--parent` are supported unchanged on
both `issue create` and `issue edit`, and the token carries the `project`
scope.

🔴 **Attach a project with `gh project item-add`, never `gh issue edit
--add-project` (#7952).** `--add-project "<title>"` exits 0 and attaches
nothing when the title does not resolve in the scope gh derives from the
repository — several projects share the title, or the project belongs to a
different owner. The exit code says it worked; `gh issue view --json
projectItems` says it did not. The owner-and-number form
(`gh project item-add <number> --owner <owner> --url <issue-url>`) names
exactly one project and attached first try on every observed case.

**Choosing the milestone — a stated rule, in order:**

1. The parent's milestone, when the issue is a sub-issue.
2. Otherwise the owning crate's backlog milestone — `Backlog · <crate>`.
3. Otherwise the one `tm issue standard` names for the repository.

Never invent a title. If none of the three yields a milestone, file with none
and post a `no-milestone: <reason>` comment on the issue in the same dispatch.
That comment is the only thing that makes an unset milestone legitimate. When
the repository has no usable milestone set, the comment carries a proposal —
"Every issue: milestone, relationships, state".

An issue holds many labels and exactly one milestone, so a workstream or a theme
parked there evicts the real slot. `ws/<session-name>` is always a label.

## Projects

Pick an issue's project by crate/topic fit from the list `tm issue standard`
prints. `agents.ticketing.default_project` names one when the project always
applies; when it does not, pick from the live list rather than guessing a title.

```bash
# -L is required: gh silently caps the list at 30 without it (#7067)
gh project list --owner <owner> -L 200 --format json  # what `tm issue standard` reads
# attach by number + owner — see "#7952" above for why the title form no-ops
gh project item-add 25 --owner bobmatnyc --url https://github.com/…/issues/7067
```

A project is a view; a milestone is a delivery slot. An issue can sit in several
projects and still carry exactly one milestone.

**Checking a filing.** One command says whether both landed:

```bash
gh issue view 7067 --json milestone,projectItems
```

A `null` milestone with no `no-milestone` comment, or an empty `projectItems`,
is the violation to fix — on the issue you just filed, before reporting it done.

Run `tm issue audit <N>` after filing and paste its output into your report — it
checks the project, the milestone and the component label mechanically and exits
1 on a violation, so the filing is proved rather than asserted (#7097).

**If `tm issue standard` prints `milestones: unavailable (…)`,** the fetch
failed; the requirement did not lift. Resolve the `gh` error, or file and say in
your report that the milestone is unset because the list could not be read.
