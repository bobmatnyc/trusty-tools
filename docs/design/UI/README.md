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
