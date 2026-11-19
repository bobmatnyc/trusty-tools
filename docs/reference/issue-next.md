# `tm issue next` — factory intake by status signal

`tm issue next` is the default intake for every factory session. It lists the
open issues that are ready to take, ranked, and it never lists an issue that is
not ready.

```bash
tm issue next [--repo OWNER/REPO] [--goal <id>] [--limit N] [--json] [--refresh]
```

`--repo` defaults to the current directory's repo. `--limit` defaults to 10.

## Rule

1. **Ready.** An issue is ready when any linked board gives it a ready Status,
   or it carries a ready status label. Either one counts.
2. **Skipped.** A ready issue is not listed when it is assigned to someone
   other than the authenticated `gh` user, or when its body has no acceptance
   criteria. The acceptance check is `has_acceptance_criteria`, the same check
   `tm issue audit` runs (TICKETING.md, "Title and body").
3. **Ranked.** Issues that serve the `--goal` come first, then by priority
   label (P0 first, unprioritized last), then by `createdAt`, oldest first.
4. **No signal.** A repo with no linked open board and no ready status label
   lists nothing and says so.
5. **Conflict.** When a ready board Status sits beside a not-ready status label,
   a ready label beside a not-ready board Status, or two boards disagree, the
   issue is still listed and is marked as a `tm issue repair` candidate.

Each result carries `signal` (`board Status: <option>`, `label <name>`, or both
joined by ` + `) and `reason` (goal, priority, created date).

## Label mapping

The mapping is detected from the labels the repo already defines. No label is
created.

| Signal | Detected from |
|---|---|
| Ready label | `status:<word>` (also `status/`, `status-`) where `<word>` is `ready`, `ready-for-dev`, `ready-for-development`, `groomed` or `todo` |
| Not-ready label | every other `status:*` label |
| Priority | `P0`-`P3`; `priority:p0`-`p3`; `priority:critical`, `high`, `medium`, `low` |
| Type (reported only) | `bug`, `enhancement`, `feature`, `documentation`, `chore`, `type:*`, `kind:*` |
| Ready board Status | the `Status` field option `Ready for Development` or `Ready` |

### Per-repo override

`~/.trusty-tools/trusty-mpm/issue-next.yaml` replaces any detected value for one
repo. Every key is optional; an unknown key is an error.

```yaml
repos:
  owner/repo:
    ready_labels: [triaged]
    not_ready_labels: [status:blocked]
    priority_labels: { sev1: 0, sev2: 1 }
    ready_status: [Todo]
```

## Goal

`--goal G01` (or `ar-G01`) reads the goals file: `$TM_GOALS_FILE`, else
`~/trusty-mpm-projects/architect/local/goals.md`. The goal's section is the
`## ...` heading that contains `(G01)`, up to the next `## ` heading. An issue
serves the goal when that section references it as `#N`, or when it carries a
`goal:G01` label. A missing file or an unknown goal is an error.

## GitHub reads and cache

One read is: one `IntakeMeta` query (viewer and linked boards), the repo's label
pages, one `IntakeBoard` query per open linked board at 100 items a page, and
the open-issue pages at 100 a page. No call names a single issue.

The snapshot is cached for 10 minutes at
`~/.trusty-tools/trusty-mpm/issue-next-cache/<owner>-<repo>.json`; the ranking
is recomputed from it on each run. A rate-limit refusal or any other read
failure exits non-zero and lists nothing. An expired cache is never used in
place of a failed read. A reply that lacks a connection the query asked for
(`repository`, `labels`, `items`, `issues`), lacks `viewer.login`, or reports
a next page with no cursor is a read failure too, and is not cached.
`--refresh` skips the cache read and rewrites the file from a fresh fetch.

The cache holds issue bodies, so its directory is created `0700` and the file
`0600`; the file is written to a temp file in the same directory and renamed
into place.

The list can be up to 10 minutes old. The consuming agent re-reads the one
issue it picks (state, assignee and board Status) before it claims it, and
drops the issue if it closed, gained another assignee, or left a ready Status.
