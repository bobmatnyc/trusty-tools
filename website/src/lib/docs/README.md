# The doc-reader seam

This directory is the extension point for the documentation reader
([#5098](https://github.com/bobmatnyc/trusty-tools/issues/5098)). The scaffold
([#5097](https://github.com/bobmatnyc/trusty-tools/issues/5097)) deliberately
ships **no** manifest parsing, Markdown rendering, navigation, or link
rewriting — only the shell those attach to. Everything below describes what
exists today and where the reader plugs in; nothing here is implemented yet.

## What the scaffold already provides

| Piece           | Where                                                       | Note                                                                                      |
| --------------- | ----------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Prerendering    | `src/routes/+layout.ts`                                     | `prerender = true` at the root, so every doc route inherits it and needs no opt-in        |
| Repo-root alias | `svelte.config.js` → `$repo`                                | Resolves to the repository root, the single declared path out of `website/`               |
| Page chrome     | `src/lib/components/SiteHeader.svelte`, `SiteFooter.svelte` | `/docs` is already in `NAV_LINKS` and highlights via `aria-current` on any `/docs/*` path |
| Anchor offset   | `src/app.css` → `html { scroll-padding-top }`               | Deep links from a doc's own table of contents clear the sticky header                     |
| Placeholder     | `src/routes/docs/+page.svelte`                              | Replace wholesale; it exists so the nav link is not a 404                                 |

## Where the reader attaches

```
src/routes/docs/
  +page.ts          NEW — load the parsed manifest, return the section/page tree
  +page.svelte      REPLACE — render the index from that tree
  [...slug]/
    +page.ts        NEW — resolve one route to its source file, render Markdown
    +page.svelte    NEW — render the page, plus prev/next from manifest order
src/lib/docs/
  manifest.ts       NEW — parse docs/public-manifest.tsv
  render.ts         NEW — Markdown → HTML, rewrite in-repo links to site routes
```

`entries` in `src/routes/docs/[...slug]/+page.ts` must export a
[`EntryGenerator`](https://svelte.dev/docs/kit/page-options#entries) driven by
the manifest, otherwise the prerenderer only discovers routes that something
already links to.

## The manifest shape

`docs/public-manifest.tsv`
([#5102](https://github.com/bobmatnyc/trusty-tools/issues/5102)) is
tab-separated, and **file order is navigation order** — do not sort it.

```
SECTION	<id>	<title>
PAGE	<section-id>	<source>	<route>	<title>
```

- `SECTION` opens a nav group; `<id>` is what a `PAGE` row references.
- `PAGE.<source>` is a repo-root-relative path to a Markdown file (reach it
  through the `$repo` alias).
- `PAGE.<route>` is the site path under `/docs`.

## The build-container constraint

`<source>` paths live outside `website/`, so the Vercel project **must** have
"Include source files outside of the Root Directory" enabled. Without it the
build container has no `../docs`, and the failure surfaces as a module-not-found
on a path that exists locally. `website/README.md` documents all three required
Vercel settings.
