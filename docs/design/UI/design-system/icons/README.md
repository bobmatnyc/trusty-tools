# Foundry Icon Set (canonical)

This directory is **the canonical source of truth** for Foundry's icon
components. It resolves the "no discrete icon set" gap noted in #3486 —
before this PR, the only real reusable icon components lived inline in one
consuming crate (`crates/trusty-agents/ui/src/lib/icons/`), with a fourth,
independently hand-drawn robot mark in that crate's `public/favicon.svg`
already drifting from it by construction.

Until #3492 ships an installable `@trusty/foundry` package, distribution is
copy-paste: crates vendor a copy of these `.svelte` files and keep them in
sync by hand. Each vendored copy carries a header comment pointing back here.
When #3492 lands, vendored copies are replaced by a real import and this
directory becomes the package source.

## Contents

| File | Purpose |
|---|---|
| `ActionIcon.svelte` | 24×24 stroke icon, selected by `name` from a fixed vocabulary (below) |
| `RobotIcon.svelte` | 32×32 robot-face brand mark, three variants (`full` / `mono` / `badge`) |
| `LogoMark.svelte` | `RobotIcon` + "Trusty Assistant" wordmark lockup, self-contained (no Tailwind dependency) |

## `ActionIcon` name → glyph vocabulary

`<ActionIcon name="..." />` renders one fixed 24×24 glyph per name. Unknown
names render an empty (but still sized) `<svg>` — callers never crash on a
typo, they just get a blank icon, which is the signal something needs fixing.

| `name` | Glyph |
|---|---|
| `pm` | Robot face (rounded square, two eyes, underscore mouth, antenna) |
| `delegate` | Arrow splitting into two branches |
| `agent` | Central node with 3 radiating dots |
| `workflow` | Three connected boxes (pipeline) |
| `terminal` | Rectangle with `>_` prompt |
| `read_file` | Document with eye overlay |
| `write_file` | Document with pencil |
| `web_search` | Magnifying glass over a globe |
| `load_skill` | Lightning bolt with a download arrow |
| `review` | Document with a checkmark |

New glyphs are additive: extend the `{#if name === '...'}` chain in
`ActionIcon.svelte`, add a row to this table, and update every vendored copy.

## `currentColor` / theme-reactivity convention

All three components default their stroke/fill color so they inherit theme
automatically, with no per-consumer light/dark branching required:

- **`ActionIcon`** defaults `color` to `currentColor` — it inherits whatever
  text color is in scope (a badge, a label, a list row), which is almost
  always what inline action icons should do.
- **`RobotIcon`** defaults `color` to `var(--trusty-primary, currentColor)`
  — it reads the Foundry accent token from
  [`design-system/tokens.css`](../tokens.css) (`--trusty-primary`), which
  itself changes value between the light (`:root`) and dark
  (`[data-theme='dark']` / `.dark`) palettes, so the mark brightens on dark
  surfaces the same way every other Foundry accent does. The `currentColor`
  fallback keeps the component usable in a context that hasn't loaded
  tokens.css. The `full`/`badge` variants' chassis/chip colors (`#2B1C12`
  panel, `#FFFFFF` details) are fixed literals, not tokens — those are
  container colors, not text-adjacent accents, and are not expected to
  invert with theme.
- **`LogoMark`** follows the same pattern for its wordmark color
  (`--trusty-text-primary`) and display face (`--trusty-display`).

Any new icon component added here should follow the same rule: default to
`currentColor` for icons meant to inherit surrounding text color, or to a
`--trusty-*` token (with a `currentColor` fallback) for icons that carry the
Foundry brand accent specifically.

## Canonical source, vendored copies

This directory is the only place icon markup should be **authored**. Do not
hand-edit a vendored crate copy and let it diverge — fix the bug or add the
glyph here first, then propagate:

- `crates/trusty-agents/ui/src/lib/icons/ActionIcon.svelte`

`crates/trusty-agents/ui/src/lib/icons/LogoMark.svelte` still uses that
crate's own Tailwind token layer (`text-foundry-text`, `font-display`)
rather than this directory's plain-CSS approach — that's an intentional,
crate-local re-expression of the same visual result, not drift; see its
header comment.

**trusty-agents is exempt from vendoring `RobotIcon.svelte`/generic
`LogoMark.svelte`/`favicon.svg` (owner-approved).** It carries its own
Trusty Agents product identity, delivered in `docs/design/UI/icons/` (the
logo lockup, standalone mark, app icon, and favicon), rather than this
directory's placeholder robot glyph. `crates/trusty-agents/ui/src/lib/icons/
{Logo,Mark}.svelte`, its `LogoMark.svelte` (re-pointed at `Mark`), and its
`public/favicon.svg` are intentionally brand-specific and diverge from this
set by design — see `docs/design/UI/icons/README.md` and each file's own
header comment. `RobotIcon.svelte` was removed from that crate entirely
(no longer vendored, no longer present) since its only two call sites were
replaced by the brand-specific components.

`docs/design/UI/design-system-svelte/src/lib/RobotMark.svelte` is a
**separate**, CSS/div-drawn robot mark from the Foundry Svelte-edition mockup
drop — it is reference-only mockup material, not part of this icon set, and
is intentionally left as-is.

## Enforcement

There is no automated sync check yet. #3492 (`@trusty/foundry` package) is
the tracked follow-up that replaces copy-paste vendoring with a real import,
which removes the drift risk structurally. Until then, PRs that touch any
file in this directory should grep for vendored copies and update them in
the same PR.
