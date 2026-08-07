# trusty-tools website

The public marketing and documentation site, deployed to Vercel from this
subdirectory. SvelteKit + Svelte 5 runes + Tailwind, themed with Foundry v2.

Cargo does not see this directory — the workspace glob in the root
`Cargo.toml` is `crates/*` — so nothing here affects a Rust build.

## Local development

```bash
cd website
pnpm install
pnpm dev        # http://localhost:5173
```

| Command             | What it does                                                   |
| ------------------- | -------------------------------------------------------------- |
| `pnpm dev`          | Dev server with HMR                                            |
| `pnpm build`        | Production build into `.vercel/output/`                        |
| `pnpm preview`      | Serve the production build locally                             |
| `pnpm check`        | `svelte-check` typecheck                                       |
| `pnpm lint`         | `prettier --check` + `eslint`                                  |
| `pnpm format`       | Rewrite with Prettier                                          |
| `pnpm test`         | Vitest: docs reader, token parity, theme store, content, build |
| `pnpm check:tokens` | Repo-wide Foundry drift gate (`scripts/check_token_drift.mjs`) |

pnpm `9.15.9`, pinned in `packageManager` to match the seven UI packages under
`crates/*/ui/`.

## Vercel project settings

Three settings must be configured on the Vercel project. Only the first is
obvious; the other two produce confusing failures when missing.

| Setting                                            | Value     |
| -------------------------------------------------- | --------- |
| Root Directory                                     | `website` |
| Include source files outside of the Root Directory | **ON**    |
| Ignored Build Step                                 | see below |

**Include source files outside of the Root Directory** is not optional. The
documentation reader ([#5098](https://github.com/bobmatnyc/trusty-tools/issues/5098))
reads `docs/public-manifest.tsv` and the Markdown it points at, both of which
live above `website/`. With the setting off the build container has no
`../docs`, and the reader fails with `FAIL NO-REPO-ROOT …` naming this exact
setting — see `src/lib/docs/README.md`.

### Ignored Build Step

```
git diff --quiet HEAD^ HEAD -- website/ docs/ Cargo.lock ':(glob)crates/*/Cargo.toml'
```

Vercel skips the build when this exits 0. Most commits in this repository touch
only Rust source, so a path filter is what keeps the deploy count sane.

**A crate release must redeploy the site.** An earlier revision of this file
filtered on `website/ docs/` alone, which is backwards for that goal: publishing
a crate bumps `crates/<name>/Cargo.toml` and `Cargo.lock` and touches neither
filtered path, so the site would have gone stale indefinitely. The two release
paths are therefore in the filter.

| Change                                       | Deploys? |
| -------------------------------------------- | -------- |
| Anything under `website/`                    | yes      |
| Anything under `docs/`                       | yes      |
| A crate version bump (`crates/*/Cargo.toml`) | yes      |
| A dependency change (`Cargo.lock`)           | yes      |
| Rust source, tests, benches                  | no       |
| CI workflows, hooks, `.claude/`              | no       |
| Root `README.md`, `CLAUDE.md`                | no       |

`Cargo.lock` over-triggers a little — a dependency bump redeploys an unchanged
site. That is the cheap direction to be wrong in, and it is what makes the
filter catch a release regardless of how the version bump was staged.

Each deploy re-pins the reader's outbound repository links to
`VERCEL_GIT_COMMIT_SHA`, so a redeploy is not a no-op even when no published
page changed: the permalinks move to the commit that was actually released.

The framework preset is SvelteKit; build command, output directory, and install
command are all detected — leave them on their defaults.

## Theme

`docs/design/UI/design-system/tokens.css` is the canonical Foundry v2 source.
`src/app.css` transcribes its hex values into Tailwind's space-separated
`R G B` triple convention, consumed as `rgb(var(--color-*) / <alpha-value>)`.
That form is required: a bare `var(--color-*)` silently generates no rule for
an opacity-modified utility like `bg-foundry-primary/10`.

- Light tokens: `:root` · dark tokens: `.dark`
- `src/lib/theme/index.ts` is the only writer of the `.dark` class on `<html>`,
  matching `darkMode: 'class'` in `tailwind.config.js`.
- `src/app.html` inlines a pre-paint snippet that sets the same class from the
  same `localStorage` key, so a dark-mode reader never sees a cream flash.
- `src/lib/theme/tokens.test.ts` fails on any drift from the canonical file.
  This package is not yet registered in `scripts/check_token_drift.mjs` — that
  entry lands with [#5103](https://github.com/bobmatnyc/trusty-tools/issues/5103).

### Three tokens that do not clear WCAG AA in light mode

All three are canonical values, not transcription errors, and the layout works
around them. `tokens.test.ts` pins the numbers so a future palette revision
surfaces here; `src/lib/docs/prose-contrast.test.ts` re-derives them and fails
if the documentation CSS reintroduces one.

| Pair                                           | Ratio  | Consequence                                               |
| ---------------------------------------------- | ------ | --------------------------------------------------------- |
| `--trusty-text-muted` on `--trusty-content-bg` | 3.87:1 | Small labels use `text-secondary` (5.85:1) instead        |
| `--trusty-accent` on `--trusty-surface-raised` | 4.50:1 | The raised band carries no accent-coloured body text      |
| `--trusty-warning` on `--trusty-content-bg`    | 3.18:1 | The warning token is not used for text on the page ground |

The accent one has a concrete consequence in the docs: a link on the raised
surface would be 4.50:1, so table heads use the **card** ground (5.47:1) and the
raised surface appears only under a fenced code block, which cannot contain a
link. Everything else clears AA in both themes; the accent clears the 3:1
non-text minimum, so it — never a border token (1.42:1) — carries the focus ring.

## Fonts

IBM Plex Sans, Chakra Petch, and IBM Plex Mono are self-hosted from
`static/fonts/` with their OFL licences. There is no CDN reference anywhere;
the build smoke test asserts the rendered HTML contains no `fonts.googleapis.com`
or `fonts.gstatic.com`.

These files are a byte-identical **second copy** of
`crates/trusty-agents/ui/public/fonts/`. Consolidating the two is worth doing
once a third consumer appears — [#3492](https://github.com/bobmatnyc/trusty-tools/issues/3492)
tracks replacing copy-paste distribution of Foundry assets with a real package.

## The documentation reader

`/docs` serves the 27 pages `docs/public-manifest.tsv` publishes. The manifest
is the whole boundary: the site enumerates it and never walks `docs/`, so an
unlisted file has no prerendered output and no URL.

Everything is built in Node at build time and prerendered to static HTML —
there is no runtime filesystem route, and a published page opens no connection
to any service. Relative links are rewritten to site routes when the target is
published and to commit-pinned GitHub permalinks when it is not; a link that
resolves to nothing fails the build.

`src/lib/docs/README.md` carries the link rule, the failure-code table, and the
rendering pipeline.
