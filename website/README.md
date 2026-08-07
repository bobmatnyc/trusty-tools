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
| `pnpm test`         | Vitest: token parity, theme store, site content, build smoke   |
| `pnpm check:tokens` | Repo-wide Foundry drift gate (`scripts/check_token_drift.mjs`) |

pnpm `9.15.9`, pinned in `packageManager` to match the seven UI packages under
`crates/*/ui/`.

## Vercel project settings

Three settings must be configured on the Vercel project. Only the first is
obvious; the other two produce confusing failures when missing.

| Setting                                            | Value                                           |
| -------------------------------------------------- | ----------------------------------------------- |
| Root Directory                                     | `website`                                       |
| Include source files outside of the Root Directory | **ON**                                          |
| Ignored Build Step                                 | `git diff --quiet HEAD^ HEAD -- website/ docs/` |

**Include source files outside of the Root Directory** is not optional. The
documentation reader ([#5098](https://github.com/bobmatnyc/trusty-tools/issues/5098))
reads `docs/public-manifest.tsv` and the Markdown it points at, both of which
live above `website/`. With the setting off, the build container simply does
not contain `../docs` and the build fails as a module-not-found on a path that
exists locally — see `src/lib/docs/README.md`.

**Ignored Build Step** exits 0 when nothing under `website/` or `docs/` changed
in the last commit, which tells Vercel to skip the build. Most commits in this
repository touch only Rust.

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

### Two tokens that do not clear WCAG AA in light mode

Both are canonical values, not transcription errors, and the layout works
around them. `tokens.test.ts` pins the numbers so a future palette revision
surfaces here.

| Pair                                           | Ratio  | Consequence                                          |
| ---------------------------------------------- | ------ | ---------------------------------------------------- |
| `--trusty-text-muted` on `--trusty-content-bg` | 3.87:1 | Small labels use `text-secondary` (5.85:1) instead   |
| `--trusty-accent` on `--trusty-surface-raised` | 4.50:1 | The raised band carries no accent-coloured body text |

Everything else clears AA in both themes; the accent clears the 3:1 non-text
minimum, so it — never a border token (1.42:1) — carries the focus ring.

## Fonts

IBM Plex Sans, Chakra Petch, and IBM Plex Mono are self-hosted from
`static/fonts/` with their OFL licences. There is no CDN reference anywhere;
the build smoke test asserts the rendered HTML contains no `fonts.googleapis.com`
or `fonts.gstatic.com`.

These files are a byte-identical **second copy** of
`crates/trusty-agents/ui/public/fonts/`. Consolidating the two is worth doing
once a third consumer appears — [#3492](https://github.com/bobmatnyc/trusty-tools/issues/3492)
tracks replacing copy-paste distribution of Foundry assets with a real package.

## The doc-reader seam

`src/lib/docs/README.md` documents where
[#5098](https://github.com/bobmatnyc/trusty-tools/issues/5098) attaches: the
route layout, the `$repo` alias, the manifest's TSV shape, and the build-container
constraint above. This scaffold ships no manifest parsing or Markdown rendering.
