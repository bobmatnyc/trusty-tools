# The documentation reader

Builds the 27 pages `docs/public-manifest.tsv` publishes
([#5098](https://github.com/bobmatnyc/trusty-tools/issues/5098), epic
[#5092](https://github.com/bobmatnyc/trusty-tools/issues/5092)). Everything here
runs in Node **at build time**; the published pages are static HTML that opens
no connection to anything.

## The boundary

The manifest is a security boundary, not a nav convenience. `docs/` holds
460-odd files that are mostly internal, and publication is one-way and
search-indexed.

The site **enumerates the manifest and never walks the tree**. There is no
catch-all that reads a path from the filesystem, no directory rule, and no
fallback. Concretely:

- `entries` in `src/routes/docs/[...slug]/+page.server.ts` is generated from the
  manifest, so the prerenderer emits exactly one file per `PAGE` row.
- `load` resolves a slug through `bySlug`, which is built from the same rows.
- `adapter-vercel` always provisions a catchall function; reaching it means the
  path has no prerendered file, and its bundle contains no repository to read.
  `buildDocSiteIfAvailable` turns that into a 404.

Proved by `site.test.ts` (`gives an unlisted docs/ file no page, no slug, and no
route`) and `tests/build-smoke.test.ts` (`emits no artifact and no route for an
unlisted docs/ file`).

## Modules

| File          | Responsibility                                                          |
| ------------- | ----------------------------------------------------------------------- |
| `manifest.ts` | Parses the TSV. File order is nav order — never sorted.                 |
| `links.ts`    | Classifies one relative link. Pure; all I/O is injected.                |
| `render.ts`   | remark/rehype pipeline, heading ids, TOC, link rewriting, hardening.    |
| `repo.ts`     | Repo root, filesystem probes, and the commit SHA links are pinned to.   |
| `site.ts`     | Orchestrates the two render phases and memoises the result.             |
| `errors.ts`   | `DocFailure` and its `FAIL CODE file:line: problem — remedy` rendering. |

## The link rule

A relative link resolves to a **site route** when its target is on the manifest,
to a **commit-pinned GitHub permalink** when its target exists anywhere else in
the repository, and **fails the build** otherwise.

| Target                                       | Becomes                                          |
| -------------------------------------------- | ------------------------------------------------ |
| on the manifest                              | `/docs<route>`, fragment preserved               |
| elsewhere in the repository — file           | `…/blob/<sha>/<path>`                            |
| elsewhere in the repository — directory      | `…/tree/<sha>/<path>`                            |
| `#fragment`                                  | unchanged, checked against this page's headings  |
| `other.md#fragment`                          | as above, fragment checked against `other.md`    |
| does not exist, or outside the repository    | `BROKEN-LINK` / `ESCAPES-REPO` — the build fails |
| absolute, `http:`/`https:`/protocol-relative | unchanged, `external: true`                      |
| absolute, any other scheme                   | `UNSAFE-SCHEME` — the build fails                |

Both "elsewhere in the repository" rows cover unpublished `docs/` pages **and**
targets outside `docs/` (`../../README.md`, `../../crates/…`) — neither can be a
site route, and the reader's situation is the same either way.

Why a permalink rather than dropping the link: dropping keeps the sentence ("see
`docs/specs/foo.md`") while removing every way to act on it, so the reader ends
up strictly worse off. Pinning to the SHA — never `blob/main`, which silently
retargets as lines shift — means the target is the revision the published prose
was written against. The SHA comes from `VERCEL_GIT_COMMIT_SHA`, `GITHUB_SHA`,
or `git rev-parse HEAD`; if none is available the build **fails** rather than
falling back to `main`.

## Failure codes

Every code stops the build and prints one line:
`FAIL <CODE> <file>:<line>: <problem> — <remedy>`, matching
`scripts/check_public_docs.sh`. Findings accumulate, so one build reports all of
them.

| Code                 | Meaning                                                                                                                     |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `MISSING-SOURCE`     | A `PAGE` row names a file that does not exist                                                                               |
| `DUP-ROUTE`          | Two rows claim the same URL                                                                                                 |
| `DUP-SOURCE`         | One source published twice                                                                                                  |
| `BAD-ROUTE`          | A route not starting with `/`                                                                                               |
| `ESCAPES-DOCS`       | A source outside `docs/`                                                                                                    |
| `ORPHAN-PAGE`        | A `PAGE` row before any `SECTION`                                                                                           |
| `SECTION-MISMATCH`   | A `PAGE` naming a section it does not follow                                                                                |
| `DUP-SECTION`        | Two `SECTION` rows with one id                                                                                              |
| `BAD-RECORD`         | Unknown record type, or wrong field count                                                                                   |
| `BROKEN-LINK`        | A relative link resolving to nothing                                                                                        |
| `BROKEN-ANCHOR`      | An anchor naming no heading on the target page                                                                              |
| `ESCAPES-REPO`       | A link resolving outside the repository                                                                                     |
| `ABSOLUTE-PATH-LINK` | A root-relative link, which means different things on GitHub                                                                |
| `EMPTY-LINK`         | A link with no destination                                                                                                  |
| `UNSAFE-SCHEME`      | An absolute link using a scheme this site won't publish (`javascript:`, `file:`, …) — only `http:`/`https:` are allowlisted |
| `RAW-HTML`           | An unknown element — see below                                                                                              |
| `OFF-SITE-IMAGE`     | An image this site does not serve                                                                                           |
| `NO-COMMIT-SHA`      | No commit to pin permalinks to                                                                                              |
| `NO-REPO-ROOT`       | No `docs/public-manifest.tsv` above the build directory                                                                     |

`RAW-HTML` exists because `<crate>` written in prose instead of `` `<crate>` ``
parses as an unknown HTML element and renders as **nothing** — the text
disappears from the published page. This corpus writes angle-bracket
metavariables constantly, so the gate is load-bearing, not theoretical.

## Rendering

remark-parse → remark-gfm → remark-rehype → rehype-raw → rehype-slug →
rehype-stringify. Tables are wrapped in a `div.doc-table` that scrolls
horizontally so the page never does; a fenced block's language hint is lifted
onto the `<pre>` as `data-lang` and shown as a label.

Syntax highlighting is deliberately absent: a highlighter's token palette would
need its own light/dark WCAG audit on top of the Foundry tokens, and a fenced
block already renders at 13.29:1 (light) / 11.88:1 (dark) as a single
foreground. `prose-contrast.test.ts` recomputes every pair from `app.css` and
fails if the three known-failing tokens are reintroduced.

## The build-container constraint

`<source>` paths live outside `website/`, so the Vercel project **must** have
"Include source files outside of the Root Directory" enabled. Without it the
build container has no `../docs`, and the reader fails with `NO-REPO-ROOT`
naming that exact setting. `website/README.md` documents all three required
Vercel settings.
