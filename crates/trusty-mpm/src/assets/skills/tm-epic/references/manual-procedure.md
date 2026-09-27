# Manual procedure — creating a tracker and its phases with `gh`

Every flag below is verified against `gh` 2.96.0, the installed version. No
`tm` verb wraps this yet ([#8376](https://github.com/bobmatnyc/trusty-tools/issues/8376)),
so this sequence IS the implementation. Run it from a session, in order; each
step's output is the next step's input.

Placeholders: `<owner>/<repo>`, `<effort>`, `<plan>.md`, `<session>`,
`<component>`, `<milestone>`, `<project-number>`. Take the milestone and project
titles from `tm issue standard`; never hand-type one.

## 0. Preconditions

```bash
tm issue standard                                  # milestone + project titles
git fetch origin
git log -1 --format=%H origin/main -- docs/research/<effort>/<plan>.md
```

The `git log` line must print a SHA. Empty output means the plan document is
not on `origin/main` yet — stop; creation refuses until it is. Keep that SHA:

```bash
SHA=$(git log -1 --format=%H origin/main -- docs/research/<effort>/<plan>.md)
PLAN_URL="https://github.com/<owner>/<repo>/blob/$SHA/docs/research/<effort>/<plan>.md"
```

Write the tracker body from `tracker-template.md` to a scratch file, with
`$PLAN_URL` in its `Plan:` line and the `phases` block left as the header row
only. Long bodies always go through `--body-file`, never inline.

## 1. File the tracker with a placeholder title

```bash
EPIC=$(gh issue create --repo <owner>/<repo> \
  --title "[EPIC] <the outcome, in plain words>" \
  --body-file tracker-body.md \
  --label epic --label <component> --label "ws/<session>" \
  --milestone "<milestone>" --assignee @me \
  | sed -E 's#.*/issues/##')
echo "$EPIC"
```

`gh issue create` prints the new issue's URL on stdout; the `sed` keeps the
number. An empty `$EPIC` means the create failed — read the error, do not
continue.

## 2. Edit the number into the title

```bash
gh issue edit "$EPIC" --repo <owner>/<repo> --title "[EPIC $EPIC] <the outcome, in plain words>"
```

## 3. Attach the project — `item-add`, never `--add-project`

```bash
gh project item-add <project-number> --owner <owner> \
  --url "https://github.com/<owner>/<repo>/issues/$EPIC"
```

`gh issue edit --add-project "<title>"` exits 0 and attaches nothing when the
title does not resolve in the scope gh derives from the repository
(`tm-ticketing`, "#7952"). The owner-and-number form names exactly one project.

## 4. File each phase as a sub-issue

Write each phase body from `phase-template.md` with `Part of #$EPIC, phase
<n> of <N>.` as its first line. Then, one call per phase:

```bash
P1=$(gh issue create --repo <owner>/<repo> \
  --title "[EPIC_$EPIC PHASE_1] <what this phase does>" \
  --body-file phase-1.md \
  --parent "$EPIC" \
  --label <type> --label <component> --label "ws/<session>" \
  --milestone "<milestone>" --assignee @me \
  | sed -E 's#.*/issues/##')
gh project item-add <project-number> --owner <owner> \
  --url "https://github.com/<owner>/<repo>/issues/$P1"
```

`--parent` takes the tracker's issue number and creates the native sub-issue
link in the same call. `<type>` is one of `bug`, `enhancement`, `refactor`,
`chore`, `documentation`, `epic`; `<milestone>` is the tracker's. A phase that
must wait on another takes `--blocked-by <number>` on the same `create`.

To adopt an issue that already exists as a phase: retitle it to the phase
form and link it with `gh issue edit "$EPIC" --add-sub-issue <number>` (or
`gh issue edit <number> --parent "$EPIC"`; both exist on 2.96).

## 5. Read the children back

```bash
gh issue view "$EPIC" --repo <owner>/<repo> --json subIssues \
  --jq '.subIssues.nodes[] | "\(.number)\t\(.state)\t\(.title)"'
```

On 2.96 `subIssues` is an object with a `nodes` array — `.subIssues[]` fails
with "expected an array but got: object". Each node carries `id`, `number`,
`state`, `title`, `url`. The paginated REST form
(`gh api repos/<owner>/<repo>/issues/$EPIC/sub_issues --paginate`) is the
fallback for an epic with more than the GraphQL page of children.

## 6. Regenerate the `phases` block — wholesale, never a hand edit

Build the table from step 5's output — one row per child, in plan order, with
the Gate column filled from each phase issue's `## Gate` section — into
`phases-table.md` (header rows included). First save the pre-edit body, so a
bad edit is always recoverable — this is what recovered
[#8445](https://github.com/bobmatnyc/trusty-tools/issues/8445) after the
regeneration step below once wiped it:

```bash
gh issue view "$EPIC" --repo <owner>/<repo> --json body --jq .body > body.orig
```

Then replace everything between the markers:

```bash
cp body.orig body.md
perl -e '
  my ($body_file, $table_file) = @ARGV;
  open my $tfh, "<", $table_file or die "open $table_file: $!";
  local $/;
  my $tbl = <$tfh>;
  close $tfh;
  $tbl =~ s/\n+\z//;
  open my $bfh, "<", $body_file or die "open $body_file: $!";
  my $body = <$bfh>;
  close $bfh;
  $body =~ s/(<!-- phases:start -->\n).*?(<!-- phases:end -->)/$1$tbl\n$2/s;
  print $body;
' body.md phases-table.md > body.new
```

`perl -e` reads `phases-table.md` and `body.md` as whole files and does the
substitution in memory, so the multi-line table never has to travel through a
shell scalar. The earlier `awk -v tbl="$(cat phases-table.md)"` form smuggled
the table through an `awk` `-v` assignment; on BSD `awk` — the default on
macOS, with no `gawk` installed — a `-v` scalar cannot hold an embedded
literal newline, so it threw `newline in string` and silently emitted **empty
stdout**. `gh issue edit --body-file` accepts an empty file without
complaint, so that failure wiped the epic's body outright. The `perl` form
keeps both marker lines and everything outside them untouched, so the
`deferred` and `followups` blocks survive, and it runs unchanged on both BSD
and GNU userlands.

Before pushing, confirm the generated body is real — abort on either check
failing rather than pushing a body that lost the table or lost everything:

```bash
[ -s body.new ] && grep -q '<!-- phases:start -->' body.new && grep -q '<!-- phases:end -->' body.new || {
  echo "ABORT: body.new is empty or missing a phases marker — do not push. Recover from body.orig." >&2
  exit 1
}
```

```bash
gh issue edit "$EPIC" --repo <owner>/<repo> --body-file body.new
```

Run this after any phase opens, closes, blocks or unblocks — and after
`tm issue transition` on a phase, until that command regenerates the block
itself.

## 7. Verify before reporting

```bash
gh issue view "$EPIC" --repo <owner>/<repo> --json title,labels,milestone,projectItems,subIssuesSummary
tm issue audit "$EPIC"
```

`subIssuesSummary` reports `total`/`completed`; it must equal the number of
phases filed. `tm issue audit` checks the project, milestone and component
label mechanically and exits 1 on a violation. Repeat the audit for each
phase number.

Report: the tracker number and URL on its own `ISSUE:` line, then one line per
phase (`PHASE_<n> → #<number>`), then the plan permalink.
