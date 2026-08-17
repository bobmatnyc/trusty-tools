# The changelog reader

Generates the "What's new" strip on the landing page and the whole `/whats-new`
page from the released flagship crates' own `CHANGELOG.md` files. Everything here
runs in Node **at build time**; the published pages are static HTML that reads
no file and opens no connection.

## The principle

**The CHANGELOGs are the source of truth. This module only makes them readable
and organised.** Nothing here writes to, generates, or edits a
`crates/*/CHANGELOG.md`. The site is a read-only derived view.

## Scope

Exactly the crates in `RELEASED_FLAGSHIPS` (`../site.ts`, the `FLAGSHIPS`
entries whose `Tool.released` is true): `trusty-search`, `trusty-memory`,
`trusty-mpm`, `trusty-analyze`, `trusty-review`, `trusty-git-analytics`.
`trusty-audit` is a flagship with a page and a card and is deliberately absent:
it has no release yet, and a section for it would render empty — which is the
exact state the `CHANGELOG-NO-RELEASES` gate exists to stop. The other crates'
changelogs are read in the repository by the people who need them; `/whats-new`
links to `crates/` and stops there.

## Modules

| File        | Responsibility                                                                           |
| ----------- | ---------------------------------------------------------------------------------------- |
| `parse.ts`  | One file's markdown → releases. Pure; no I/O, so every failure path is fixturable.       |
| `site.ts`   | Reads those files, applies the gates, memoises. The only entry point routes call.        |
| `errors.ts` | The failure vocabulary. Record shape and formatting are shared with `../docs/errors.ts`. |

The remark/rehype pipeline is `../docs/render.ts`. There is deliberately no
second markdown stack in this directory.

## The gates

Each of these **fails the build**. None of them renders a placeholder, an empty
array, or a "no changes yet" string — an empty What's New is indistinguishable
from "nothing shipped", and the reader has no way to tell which they are
looking at. A green build is therefore itself the evidence that all six
sections are populated.

| Code                     | Fires when                                                           |
| ------------------------ | -------------------------------------------------------------------- |
| `CHANGELOG-MISSING`      | A flagship's `CHANGELOG.md` is absent or unreadable.                 |
| `CHANGELOG-NO-RELEASES`  | The file read but parsed to zero release sections.                   |
| `CHANGELOG-BAD-RELEASE`  | A heading opens `## [` and never closes the bracket.                 |
| `CHANGELOG-EMPTY-LATEST` | The newest release carries no items under any category.              |
| `CHANGELOG-BAD-LINK`     | A link in the prose points somewhere this site cannot send a reader. |

Every finding in a build is collected and reported together, one per line, as
`FAIL <CODE> <file>:<line>: <problem> — <remedy>`.

## Links inside changelog prose

A relative link in a changelog (`[spec](../../docs/specs/foo.md)`) is written to
be followed **on GitHub**. Rendered verbatim on `/whats-new` it resolves against
the SITE root, so that href becomes `/docs/specs/foo.md` — a route this site
does not have. SvelteKit's prerenderer refuses to build with one, which is how
the three in `crates/trusty-mpm/CHANGELOG.md` surfaced.

Each is resolved against the crate's own directory and rewritten to
`blob/main/<target>`. A target that lands outside the repository, or on a path
that does not exist, is `CHANGELOG-BAD-LINK` — a build failure, with no second
chance.

**No second chance, specifically.** An earlier revision of this PR tried to
absorb an off-by-one `../` by stripping the leading `../` and accepting the
result whenever that path existed. It stripped ANY depth, so
`[the README](../../../README.md)` written in `crates/trusty-search/CHANGELOG.md`
resolved to the repo-root `README.md` and shipped a confident, wrong link with
zero failures reported. That falsifies the only guarantee this gate offers — a
green build means every link resolved AS WRITTEN. The one real off-by-one in the
corpus (`crates/trusty-mpm/CHANGELOG.md` line 1982) was corrected at the source
instead, which is what a typo deserves.

## The one thing that recovers instead of failing

`<path>` written outside backticks parses as an unknown HTML element and renders
as **nothing** — `run trusty-search index <path>` would publish as
`run trusty-search index`. Eight of these are in the corpus today.

The doc reader fails the build on the identical hazard. This module does not,
and the difference is who can fix it: a published page's author can add the
backticks, but **a changelog is append-only history this site must not edit**, so
a gate here would be unfixable without violating the principle at the top of
this file. The element is turned back into the literal text the author wrote and
its contents are kept, so nothing is lost — `parse.test.ts` pins all four real
sites.

## The link-reference trap

`crates/trusty-search/CHANGELOG.md` ends with 58 link-reference definitions
pointing at the **pre-monorepo** repository:

```
[0.3.36]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.35...v0.3.36
```

Those labels match the `## [0.3.36]` heading text exactly, so remark resolves
every version heading into a link to a repository that no longer exists.
`crates/trusty-analyze/CHANGELOG.md` has 13 more of the same shape, pointing at
the right repository — still stripped, because a version heading should not be
a link at all.

`stripLinkDefinitions` blanks each definition line (rather than deleting it, so
reported line numbers still match the file on disk) before parsing.
`parse.test.ts` proves both halves: the untouched file really does render
anchors, and the stripped one renders plain text.

## The grammar, as the corpus actually writes it

Keep a Changelog describes `## [<semver>] — <YYYY-MM-DD>` and `### <Category>`.
The 8 400 lines of hand-written history in these six files deviate in four ways,
each of which a strict parser would silently discard:

- **49 release headings carry a title where the date should be** —
  `## [0.1.46] — 4 indexing speed optimizations`. Three carry neither a semver
  nor a date: `## [consolidation] — 2026-05-26`, `## [0.4.0] and prior`,
  `## [2026-05-11]`. The date is optional; the bracketed label is not.
- **Roughly 40 category headings carry a qualifier** —
  `### Fixed (closes #1373)`, `### Added — Phase 2 (…)`, `### Deprecated: … `.
  Bucketing is by the **leading word**; the full heading is kept as the label.
- **Categories outside the assembler's canonical set** (`Breaking Added Fixed
Performance Changed Removed Security Documentation`, from
  `scripts/assemble-changelog.sh:87`) are hand-written history, not errors.
  `Notes`, `Internal`, `Highlights` and friends render under their literal
  heading.
- **A release body is not only bullets.** 114 paragraphs, 42 table rows, 40
  fenced blocks and 15 block quotes sit under category headings, and four crates
  open their newest release with prose before the first category. All of it is
  kept, in source order. The only node dropped is the `---` rule between
  releases, which is structure rather than content.

## The unpinned link, on purpose

The doc reader pins every in-repo link to a commit SHA (`blob/<sha>`) so a
cited line cannot drift out from under the prose quoting it. The changelog
links are the opposite case and use `blob/main`: they point at a **living**
document, and a reader who follows "the full changelog" wants the one that is
current now. The reasoning is restated at both link-building sites so nobody
"fixes" them into permalinks.
