# Project #45 views and workflows capture

Read-only GraphQL capture of GitHub Project [#45](https://github.com/users/bobmatnyc/projects/45) (user `bobmatnyc`, title `trusty-tools-master`), taken on 2026-10-25. It satisfies step 1 of the Preparation section in [plan.md](plan.md) (plan PR [#10268](https://github.com/bobmatnyc/trusty-tools/pull/10268), commit [bcd26bbdc](https://github.com/bobmatnyc/trusty-tools/commit/bcd26bbdc9382da16a0695e4ea0f70c95db93801)). It feeds the Phase 1 board spec, [#10275](https://github.com/bobmatnyc/trusty-tools/issues/10275), of epic [#10274](https://github.com/bobmatnyc/trusty-tools/issues/10274).

Every query was a read. Project #45 was not changed. Every connection returned one page (`hasNextPage` false). No query needed adjustment after the first run.

## Queries

Each query ran as `gh api graphql -F query=@<file>`. The three paged queries take an optional `$cursor`, which was left unset.

Project metadata (`project.graphql`):

```graphql
query {
  user(login: "bobmatnyc") {
    projectV2(number: 45) {
      id
      title
      shortDescription
      public
      readme
      closed
      template
    }
  }
}
```

Views (`views.graphql`):

```graphql
query($cursor: String) {
  user(login: "bobmatnyc") {
    projectV2(number: 45) {
      views(first: 50, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          name
          number
          layout
          filter
          groupByFields(first: 20) { nodes { ... on ProjectV2FieldCommon { name } } }
          verticalGroupByFields(first: 20) { nodes { ... on ProjectV2FieldCommon { name } } }
          sortByFields(first: 20) { nodes { direction field { ... on ProjectV2FieldCommon { name } } } }
          fields(first: 50) { nodes { ... on ProjectV2FieldCommon { name } } }
        }
      }
    }
  }
}
```

Workflows (`workflows.graphql`):

```graphql
query($cursor: String) {
  user(login: "bobmatnyc") {
    projectV2(number: 45) {
      workflows(first: 50, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { id name number enabled }
      }
    }
  }
}
```

Fields (`fields.graphql`):

```graphql
query($cursor: String) {
  user(login: "bobmatnyc") {
    projectV2(number: 45) {
      fields(first: 50, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          __typename
          ... on ProjectV2FieldCommon { name dataType }
          ... on ProjectV2SingleSelectField { options { id name color description } }
          ... on ProjectV2IterationField { configuration { duration startDay iterations { id title startDate duration } completedIterations { id title startDate duration } } }
        }
      }
    }
  }
}
```

Workflow type introspection (`workflow-schema.graphql`):

```graphql
query {
  __type(name: "ProjectV2Workflow") { fields { name type { name kind ofType { name } } } }
}
```

Mutation type introspection (`mutation-schema.graphql`):

```graphql
query {
  __type(name: "Mutation") { fields { name } }
}
```

## Project metadata

| Property | Value |
|---|---|
| Title | `trusty-tools-master` |
| Node id | `PVT_kwHOAA3u1M4BllRd` |
| Public | true |
| Closed | false |
| Template | false |
| shortDescription | "Every open trusty-tools issue. Status: Backlog → Groomed → In development → Development complete → Merged → Deployed → Published." |
| readme | A status-lifecycle table, a conventions list and a dated progress log (see below) |

The readme opens with a statement that the board tracks every open issue in `bobmatnyc/trusty-tools`, that new open issues are added automatically and start at Backlog, and that pull requests are not tracked. It then carries a seven-row Status table, a Conventions list and a Progress log. The newest progress entry is dated 2026-10-09 and counts items by the new Status names.

## Views

| # | Name | Layout | Filter | groupBy | verticalGroupBy | sortBy |
|---|---|---|---|---|---|---|
| 1 | List | `TABLE_LAYOUT` | empty | none | none | none |
| 2 | Swimlanes | `BOARD_LAYOUT` | empty | none | Status | none |
| 3 | Roadmap | `ROADMAP_LAYOUT` | empty | none | none | none |

Visible fields, in the order the API returned them:

| # | View | Visible fields |
|---|---|---|
| 1 | List | Title, Assignees, Status, Linked pull requests, Milestone, Sub-issues progress, Created, Start date, Target date, Crate, Deployed on, Published on |
| 2 | Swimlanes | Title, Assignees, Status, Linked pull requests, Sub-issues progress |
| 3 | Roadmap | Title, Assignees, Status, Linked pull requests, Sub-issues progress |

All three `sortByFields` lists are empty, so no sort direction exists to record. The views query does not read the Roadmap's date-field mapping or zoom level, and does not read column widths or column order beyond the field list above.

## Workflows

| # | Name | Enabled | Filter or repo |
|---|---|---|---|
| 1 | Item closed | false | not exposed by the schema |
| 2 | Pull request merged | false | not exposed by the schema |
| 3 | Auto-close issue | false | not exposed by the schema |
| 5 | Pull request linked to issue | false | not exposed by the schema |
| 6 | Item added to project | false | not exposed by the schema |
| 7 | Auto-add sub-issues to project | true | not exposed by the schema |
| 8 | Auto-add to project | true | not exposed by the schema |

Number 4 is absent from the response. Only workflows 7 and 8 are enabled.

`ProjectV2Workflow` exposes these fields and no others: `createdAt`, `enabled`, `fullDatabaseId`, `id`, `name`, `number`, `project`, `updatedAt`. It has no filter, trigger, action or repository field. The filter and target repository of the two enabled auto-add workflows cannot be read over GraphQL.

## Fields

The project has 20 fields: 12 built-in and 8 custom. No iteration field exists.

| Field | Data type | Kind |
|---|---|---|
| Title | TITLE | built-in |
| Assignees | ASSIGNEES | built-in |
| Status | SINGLE_SELECT | custom |
| Labels | LABELS | built-in |
| Linked pull requests | LINKED_PULL_REQUESTS | built-in |
| Milestone | MILESTONE | built-in |
| Repository | REPOSITORY | built-in |
| Reviewers | REVIEWERS | built-in |
| Parent issue | PARENT_ISSUE | built-in |
| Sub-issues progress | SUB_ISSUES_PROGRESS | built-in |
| Created | CREATED | built-in |
| Updated | UPDATED | built-in |
| Closed | CLOSED | built-in |
| Start date | DATE | custom |
| Target date | DATE | custom |
| Crate | SINGLE_SELECT | custom |
| Deployed on | DATE | custom |
| Published on | DATE | custom |
| Hold | SINGLE_SELECT | custom |
| Agents | TEXT | custom |

Status options, in order:

| # | Name | Color | Description | Id |
|---|---|---|---|---|
| 1 | Backlog | GRAY | Issue created | `f75ad846` |
| 2 | Ready for Development | BLUE | Selected for work | `b8a5db32` |
| 3 | In Development | YELLOW | Engineer session claimed it | `47fc9ee4` |
| 4 | In PR | ORANGE | PR open, `Refs #N` | `92d8f3aa` |
| 5 | On Preview | YELLOW | Per-PR preview deploy is up and checked | `93887173` |
| 6 | Merged | GREEN | PR squash-merged | `0a17db94` |
| 7 | On Prod | PINK | Runs in production or as the installed binary, with live evidence | `21c81169` |
| 8 | Published | PURPLE | Version on crates.io | `98236657` |
| 9 | Won't Do | GRAY | Not planned, duplicate or superseded, with a link | `691cb276` |

Hold options, in order:

| # | Name | Color | Description | Id |
|---|---|---|---|---|
| 1 | Blocked | RED | External wait; blocker named | `a5a692be` |
| 2 | Needs decision | ORANGE | Waits on a named decision-maker | `3fd5c848` |

Crate options, in order. All 11 are GRAY with an empty description.

| # | Name | Id |
|---|---|---|
| 1 | trusty-mpm | `4e86ceec` |
| 2 | trusty-code | `1a39da1f` |
| 3 | trusty-agents | `2c9a3131` |
| 4 | trusty-agents-common | `cd9e9b83` |
| 5 | trusty-memory | `794e5f63` |
| 6 | trusty-search | `3283a327` |
| 7 | trusty-common | `8256c9f0` |
| 8 | trusty-review | `0cc4b2ed` |
| 9 | trusty-analyze | `0a080d2f` |
| 10 | trusty-embedderd | `df1339c0` |
| 11 | trusty-console | `2f11acab` |

Option ids are informational. The spec keys on names.

## Cross-check against plan.md

Compared with the "Research findings" bullet on Project #45 fields in [plan.md](plan.md) (the REST read, on `origin/main` after PR [#10268](https://github.com/bobmatnyc/trusty-tools/pull/10268)). Matches first, then every difference.

Matches:

- Status has 9 options. Names, order and colors equal the plan: Backlog GRAY, Ready for Development BLUE, In Development YELLOW, In PR ORANGE, On Preview YELLOW, Merged GREEN, On Prod PINK, Published PURPLE, Won't Do GRAY.
- Every Status option carries a non-empty description, as the plan states.
- Crate has 11 options, as the plan states.
- Hold has Blocked RED and Needs decision ORANGE.
- Date fields Deployed on, Published on, Start date and Target date exist, all `DATE`.
- Text field Agents exists.
- The readme uses older stage names.

Differences:

1. The plan names 8 custom fields (Status, Crate, Hold, four dates, Agents). The capture finds the same 8 and also 12 built-in fields the plan does not mention: Title, Assignees, Labels, Linked pull requests, Milestone, Repository, Reviewers, Parent issue, Sub-issues progress, Created, Updated, Closed. These are GitHub-provided, not custom.
2. The plan lists the date fields as Deployed on, Published on, Start date, Target date. The board orders them Start date, Target date (before Crate), then Deployed on, Published on (after Crate).
3. The plan does not list the Status descriptions, the Crate option names, the Crate colors or the Hold descriptions. The capture records them above. Crate options are all GRAY with empty descriptions.
4. The plan says the README uses older stage names. The `shortDescription` uses the same older names; the plan does not mention it.
5. The plan's source is a REST read; this capture is GraphQL. The two agree on every item the plan states.

No difference in option count, name, color or order for Status, Crate or Hold.

## Mutation schema

The `Mutation` type has these ProjectV2 mutations:

`addProjectV2DraftIssue`, `addProjectV2ItemById`, `archiveProjectV2Item`, `clearProjectV2ItemFieldValue`, `convertProjectV2DraftIssueItemToIssue`, `copyProjectV2`, `createProjectV2`, `createProjectV2Field`, `createProjectV2IssueField`, `createProjectV2StatusUpdate`, `createProjectV2View`, `deleteProjectV2`, `deleteProjectV2Field`, `deleteProjectV2Item`, `deleteProjectV2StatusUpdate`, `deleteProjectV2View`, `deleteProjectV2Workflow`, `linkProjectV2ToRepository`, `linkProjectV2ToTeam`, `markProjectV2AsTemplate`, `unarchiveProjectV2Item`, `unlinkProjectV2FromRepository`, `unlinkProjectV2FromTeam`, `unmarkProjectV2AsTemplate`, `updateProjectV2`, `updateProjectV2Collaborators`, `updateProjectV2DraftIssue`, `updateProjectV2Field`, `updateProjectV2ItemFieldValue`, `updateProjectV2ItemPosition`, `updateProjectV2StatusUpdate`, `updateProjectV2View`.

For views: `createProjectV2View`, `updateProjectV2View` and `deleteProjectV2View` exist. For workflows: `deleteProjectV2Workflow` exists. No `createProjectV2Workflow` or `updateProjectV2Workflow` exists.

This is a schema answer only. The capture did not introspect the view mutations' input types and did not call any mutation. Whether the view mutations work, and which view properties they set (layout, filter, grouping, visible fields), is for the Phase 2 trial in [plan.md](plan.md) to decide.

## What this means for the Phase 1 spec

Facts the spec ([#10275](https://github.com/bobmatnyc/trusty-tools/issues/10275)) can declare from this capture:

Views. Three, all with an empty filter, no group-by on rows and no sort:

- `List`: layout TABLE. Visible fields in order: Title, Assignees, Status, Linked pull requests, Milestone, Sub-issues progress, Created, Start date, Target date, Crate, Deployed on, Published on. Crate is a repo extra per decision D6 in the plan, so a board without it has a shorter List.
- `Swimlanes`: layout BOARD. Vertical group-by Status. Visible fields: Title, Assignees, Status, Linked pull requests, Sub-issues progress.
- `Roadmap`: layout ROADMAP. Visible fields: Title, Assignees, Status, Linked pull requests, Sub-issues progress. Its date-field mapping and zoom were not read.

Workflow intents. Two are on, five are off:

- On: auto-add issues (workflow 8, "Auto-add to project") and auto-add sub-issues (workflow 7, "Auto-add sub-issues to project").
- Off: Item closed, Pull request merged, Auto-close issue, Pull request linked to issue, Item added to project.

Cannot be read over the API:

- The filter and target repository of the two auto-add workflows.
- The Roadmap's date-field mapping and zoom level, and any view column widths. The queries did not request them.

Cannot be written over the API, from the schema:

- Workflows cannot be created or updated. They can only be deleted. Enabling auto-add needs the UI or the `actions/add-to-project` Action, as the plan's Known limits state.
- Views have create, update and delete mutations. Whether they work is not proven; the Phase 2 trial decides it.

## Observed vocabulary drift

The project `shortDescription` and `readme` still use the old Status vocabulary: Backlog, Groomed, In development, Development complete, Merged, Deployed, Published. The Status field options use the new names: Backlog, Ready for Development, In Development, In PR, On Preview, Merged, On Prod, Published, Won't Do. The readme's newest progress entry (2026-10-09) already counts by the new names, so the text is mixed inside one document.

Project #45 is unchanged by this capture.
