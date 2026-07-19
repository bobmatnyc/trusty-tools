# Trusty Tools UI Design System

## Foundry Design System (v1)

Foundry is the authoritative design system for trusty-* UIs (trusty-search, trusty-memory, trusty-analyze, trusty-mpm-gui, trusty-console).

### Contents

- **[design-system/README.md](design-system/README.md)** — Design philosophy, guardrails, and component quick-reference for Foundry v1
- **design-system/tokens.css** — CSS custom properties (--trusty-*) for colors, typography, spacing, and layout
- **design-system/foundry.css** — Component styles (.btn, .card, .badge, .modal, .toast, etc.) drop-in for Svelte UIs

All files are verbatim from the 2026-07-18 design drop. No modifications or renames have been applied.

### Standards & Specifications

See [docs/specs/trusty-code-harness-ui.md](../../specs/trusty-code-harness-ui.md) (DOC-39) §8 — visual system. Note that DOC-39's visual-system section will be reconciled with Foundry in a follow-up; see issue #3153 for progress.

### Installation

Drop both CSS files into each crate's `ui/src/lib/styles/` directory:

1. Replace `tokens.css` with this version
2. Merge or replace `global.css` with `foundry.css`
3. Update the Google Fonts link in `ui/index.html` (see design-system/README.md for the exact URL)

### Related Issues

- #3153 — Reconcile Foundry with DOC-39 visual-system spec
- #3133 — Design system request (resolved by this drop)
