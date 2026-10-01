# Foundry shared components (canonical)

Svelte 5 components shared by more than one trusty-* UI. This directory is
the source of truth; edit here first.

| File | Purpose |
|---|---|
| `codeView.js` | Pure helpers: `escapeHtml`, the extension → highlight.js language map, `renderCode(hljs, source, path)`, `diffLineKind`, `gutterNumbers` |
| `CodeView.svelte` | Highlighted source block (`pre.file-code`), with an optional line-number gutter (`startLine`) |
| `DiffView.svelte` | Unified diff, one tinted line per `div` |

The components came out of the agents GUI file viewer
(`crates/trusty-agents/ui/src/components/FileViewer.svelte`). `codeView.js`
imports no package: the caller passes its own highlight.js instance, so the
file resolves from any package.

## Theming

Colours come from CSS custom properties, falling back to the Foundry tokens:

| Property | Fallback |
|---|---|
| `--code-accent` | `--trusty-accent` (keywords, numbers, diff hunk headers) |
| `--code-string` | `--trusty-success` (strings, titles) |
| `--code-added-bg` | `--trusty-success-soft` |
| `--code-removed-bg` | `--trusty-danger-soft` |
| `--code-border` | `--trusty-border` (gutter rule) |

A Tailwind app whose tokens are `--color-*` RGB triples sets the
`--code-*` properties on a wrapper, as the agents file viewer does.

## Consumers

**Imported, not copied:**

- `crates/trusty-console/ui-search` — through the `@foundry` alias in its
  `vite.config.js` (search hit viewer). That package ships only its built
  bundle, so the import never has to reach a crate tarball.

**Vendored byte-for-byte:**

- `crates/trusty-agents/ui/src/lib/foundry/` — trusty-agents publishes
  `ui/src` and runs `pnpm build` at install time (`build.rs`, #8094), and a
  crates.io tarball cannot carry a file from outside the crate. The copy is
  checked by `src/lib/foundry/vendored.test.ts`, which fails on any drift.
  After editing a file here, copy it there in the same change.

The search bundle rows in `scripts/ui-bundle-manifest.tsv` list this directory
in column 4 (`shared_dirs`), so an edit here makes both search bundles stale in
`scripts/check-ui-bundle-freshness.sh`. Rebuild them with
`make -C crates/trusty-console search-ui`, then
`make -C crates/trusty-search sync-ui`.
