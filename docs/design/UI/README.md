# Trusty Tools UI Design System

## Foundry Design System (v2)

Foundry is the authoritative design system for trusty-* UIs (trusty-search, trusty-memory, trusty-analyze, trusty-mpm-gui, trusty-console). v2 adds the Night Shift dark theme and sample implementation screens.

### Core Files

- **[design-system/README.md](design-system/README.md)** — Design philosophy, guardrails, and component quick-reference
- **design-system/tokens.css** — CSS custom properties (--trusty-*) for both light and dark palettes; activate dark theme with `<html data-theme="dark">` or `.dark` class
- **design-system/foundry.css** — Component styles (.btn, .card, .badge, .modal, .toast, etc.) drop-in for Svelte UIs

### Design Documentation

- **Foundry Spec.dc.html** — 12-section component specification (typography, color, buttons, forms, tables, cards, etc.)
- **Foundry Screens.dc.html** — Sample dashboard, search/chat, indexes, and health screen implementations
- **Foundry Ecosystem.dc.html** — Visual brand identity, robot mark variations, and application grid (trusty-search, trusty-memory, trusty-analyze, trusty-mpm, trusty-console)
- **Foundry Robot Animations.dc.html** — Idle robot mark and loading animations
- **Trusty Design Directions.dc.html** — Design direction sketches and evolution narrative
- **Current UI.dc.html** — Capture of pre-Foundry UI state (legacy reference)
- **support.js** — Shared utility script loaded by all .dc.html documents

All files are verbatim from the 2026-07-18 design drop (v2). No modifications or edits have been applied.

### Key v2 Changes

- **Dark theme (Night Shift)** is now normative. Activate with `data-theme="dark"` attribute on `<html>` or `.dark` class; tokens.css ships both light and dark palettes under `:root` and `[data-theme='dark'], .dark` selectors.
- **Sample implementation screens** — Dashboard, search + chat, indexes, health monitoring — show Foundry in action with real component combinations.
- **Robot ecosystem mark** — visual grid showing the numbered UNIT identity for each tool (01 Search, 02 Memory, 03 Analyze, …).

### Standards & Specifications

See [docs/specs/trusty-code-harness-ui.md](../../specs/trusty-code-harness-ui.md) (DOC-39) §8 — visual system. Reconciliation with DOC-39 is tracked in issue #3153.

### Installation

Drop both CSS files into each crate's `ui/src/lib/styles/` directory:

1. Replace `tokens.css` with this version (includes dark palette)
2. Merge or replace `global.css` with `foundry.css`
3. Update the Google Fonts link in `ui/index.html` (see design-system/README.md for the exact URL)
4. To enable dark theme: add `data-theme="dark"` to `<html>` based on system preference or user setting

### Related Issues

- #3153 — Reconcile Foundry with DOC-39 visual-system spec
- #3133 — Design system request (resolved)

---

## Foundry — Svelte Edition (reference)

**[design-system-svelte/](design-system-svelte/)** is a runnable Svelte 5 (runes) + Vite
port of the same Foundry v2 tokens, landed verbatim from the 2026-07-19 design drop. It
is committed as **reference material**, not as an importable package — see rationale
below.

### What it is

- `design-system-svelte/src/styles/tokens.css` and `foundry.css` are **byte-identical**
  to `design-system/tokens.css` / `design-system/foundry.css` — confirms the Svelte
  edition is a straight port of the same v2 tokens/visuals, not a new palette.
- `design-system-svelte/src/lib/` — reusable primitives built on those raw `foundry.css`
  classes: `Button`, `Badge`, `Modal`, `Toast`, `StatCard`, `Sidebar`, `Topbar`,
  `AppShell`, `RobotMark` (idle/receiving/working states, square/round/visor eyes).
- `design-system-svelte/src/screens/` — 14 full-page reference screens across
  search (Dashboard, Search+chat, Indexes, Health, Dialogs gallery), console
  (Command deck), memory (Palaces), agents (Chat, Projects, Auth gate, Task failure,
  Recap), and **code** (`CodeGui.svelte`, `CodeTui.svelte` — direct mockups for
  `trusty-code-gui`). Screens are fixed 1440×900 review frames with static mock data,
  by the drop's own README — not drop-in production components.
- Svelte 5 runes throughout (`$state`, `$props`, `{@render children?.()}`); no
  Svelte 4 idioms found.
- No licensing/provenance markers beyond the drop's own README; treated as
  Bob-authored internal design material like the rest of `docs/design/UI/`.

### Why reference, not `crates/trusty-code-gui/ui/src` source

`crates/trusty-code-gui/ui` already has its own, tested Foundry integration
(issue #3153): Tailwind utility classes (`bg-trusty-surface`, `text-status-ok/60`,
etc.) backed by `rgb(var(--color-*) / <alpha-value>)` CSS custom properties in
`src/app.css`, wired through `tailwind.config.js`. The Svelte-edition components use a
**different** mechanism — plain `foundry.css` classes (`.btn`, `.card`, `.badge`) and
raw `--trusty-*` vars consumed directly in inline styles/class bindings. Importing
`foundry.css` wholesale into the app's style entry point would stand up a second,
overlapping styling system (duplicate `html`/`body` resets, `.btn`/`.card` class names)
rather than a drop-in addition — that's a restyle, not an additive change, so it's out
of scope for this PR. Landing the drop as a runnable, browsable reference
(`npm install && npm run dev` inside `design-system-svelte/`) preserves its full value
— including the `CodeGui`/`CodeTui` screens as a north star for trusty-code-gui's own
layout — without destabilizing the shipped Tailwind-token integration.

### Adoption plan (follow-up work, not this PR)

Component-by-component adoption into `crates/trusty-code-gui/ui` means re-expressing
each `design-system-svelte/src/lib/` primitive against the app's existing
`trusty-*`/`status-*` Tailwind tokens (same visual values, different class mechanism),
not copying the files as-is. Roughly sized, smallest/lowest-risk first:

1. **RobotMark** (S) — self-contained, inline-styled, zero foundry.css class
   dependency beyond animation keyframes; easiest first port and gives the app its
   brand mark.
2. **Badge / Button** (S) — small, high-reuse primitives; mechanical class-to-Tailwind
   translation (`.btn-primary` → `bg-trusty-primary text-trusty-text-inverse …`).
3. **StatCard / Toast** (M) — a few more states/variants to cover, still self-contained.
4. **Modal** (M) — needs the app's existing focus-trap/overlay conventions reconciled
   with the drop's markup.
5. **Sidebar / Topbar / AppShell** (L) — chrome components; trusty-code-gui already has
   its own shell (`AppHeader.svelte`, `WorkstreamRail.svelte`, `ServiceNav.svelte`), so
   this is a design-reconciliation task, not a straight port — do last, informed by
   whatever the smaller ports teach about the token mapping.
6. **Screens** — reference only; use `CodeGui.svelte`/`CodeTui.svelte` as a layout
   guide when reworking trusty-code-gui's own tabs, not as source to import.

### Related Issues

- #3153 — Reconcile Foundry with DOC-39 visual-system spec (established the Tailwind
  token bridge this drop's components don't yet use)
