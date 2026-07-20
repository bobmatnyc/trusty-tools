# Foundry — Trusty Suite Design System (v1)

Robot-themed, rust-colored design system for the trusty-* ecosystem
(trusty-search, trusty-memory, trusty-analyze, trusty-mpm-gui, trusty-console).

See [`icons/README.md`](icons/README.md) for the canonical icon set
(`ActionIcon`, `RobotIcon`, `LogoMark`) — the single source of truth crate
copies must stay in sync with.

## Install

Drop-in for the existing Svelte UIs — variable names are unchanged:

1. Replace `crates/<crate>/ui/src/lib/styles/tokens.css` with `tokens.css`.
2. Replace (or merge) `global.css` with `foundry.css` — it restyles the same
   class names (.btn, .card, .table, .badge, .stat, .modal, …) plus new ones
   (.toast, .btn-ghost, .display).
3. Swap the Google Fonts link in each `ui/index.html` to:

   https://fonts.googleapis.com/css2?family=Chakra+Petch:wght@500;600;700&family=IBM+Plex+Sans:wght@400;500;600;700&family=IBM+Plex+Mono:wght@400;500;600&display=swap

## Design philosophy

Foundry is a workshop, not a showroom.

- **Machines talk mono.** Anything a computer produced or consumes — ids,
  paths, counts, versions, table headers, labels — is IBM Plex Mono
  (uppercase, letterspaced for labels). Human prose is IBM Plex Sans.
  Chakra Petch is reserved for headings and hero numbers.
- **Rust is the signal, paper is the field.** #B7410E marks the ONE thing
  that matters on a surface: the primary action, the highlighted datum,
  the active nav item. If two things are rust, neither is.
- **Flat and honest.** Depth comes from clean 1.5px line borders and the
  raised #EFE6D8 header strip — never drop shadows on resting surfaces.
  Shadows belong only to things that float: modals, toasts, menus.
- **The robot is a colleague.** The UNIT mark appears at brand moments —
  sidebar brand, empty states, chat avatar — never as decoration on every
  card. Each tool is a numbered UNIT (01 Search, 02 Memory, 03 Analyze…).

## Guardrails (extend without breaking)

DO
- Use only the custom properties in tokens.css. Need a new tint? Derive it
  in oklch by shifting lightness only, keeping the rust hue (~40deg) and
  chroma family.
- Give every button tier a distinct background (solid rust / card / raised /
  soft-rust / transparent). Never two transparent buttons side by side.
- Pair every status color with a text label or icon — color is never the
  only carrier of state.
- Keep radii at 3/5/8px, borders at 1px (dividers) / 1.5px (containers),
  spacing on the 4px grid.
- Badges are rectangular stamps (4px radius), mono uppercase 10px.
- Charts: rectangular bars only, one rust hue family per chart, the
  highlighted datum takes full #B7410E, values are mono stamps.

DO NOT
- No pills, gradients, curved charts, emoji, or offset "sticker" shadows.
- No new accent hues. Success (#3F6F2A), warning (#B07D10), danger
  (#C2331F) and info (#3D6B8A) are fixed; everything else stays in the
  rust-to-paper ramp.
- No rust on more than one element per view region; never rust body text
  or large rust background areas.
- No new fonts or weights. Three faces, 400-700, nothing below 10px mono /
  12px sans.
- No zebra-striped tables; row hover uses --trusty-surface-hover.

## Dark theme ("Night Shift")

tokens.css ships both palettes. Light is :root; dark activates via
`<html data-theme="dark">` (or a `.dark` class). Rules:

- Same rust hue family in both themes; the accent brightens one step on
  dark (#B7410E -> #D97742) to hold contrast.
- Soft status fills become translucent overlays on dark (never opaque
  pastels on oxide surfaces).
- Components reference tokens only — a component that hardcodes a hex
  breaks theming and fails review.

## Component states quick-reference

- Buttons: PRIMARY solid rust · SECONDARY card bg · TERTIARY raised bg ·
  DANGER soft-rust bg · GHOST transparent · DISABLED raised bg + muted text.
- Toasts: dark chassis (#2B1C12) panels, bottom-right stack, 3px status
  left edge, mono title + sans body. Success auto-dismisses at 5s; errors
  persist until dismissed.
- Modals: card with raised header strip, Chakra Petch title, actions
  right-aligned in footer (destructive confirm = primary rust).
- Empty states: muted idle-robot mark, one mono label, one line of sans
  guidance, one primary action.
