# Foundry — Trusty Tools screens in Svelte

Svelte 5 (runes) + Vite build of the Foundry design system screens.

## Run

```sh
npm install
npm run dev
```

Open the printed URL. A left rail switches between all 14 screens.

## Layout

- `src/styles/tokens.css` — Foundry design tokens (light + `.dark` / `[data-theme='dark']` NIGHT SHIFT).
- `src/styles/foundry.css` — global component classes (`.btn`, `.badge`, `.card`, `.table`, `.toast`, `.modal`, robot animations…). Same class names as the Trusty Suite UIs.
- `src/lib/` — shared components:
  - `RobotMark.svelte` — the robot mark (size, body/face colors, `eyes: square|round|visor`, `state: idle|receiving|working`).
  - `Badge.svelte`, `Button.svelte`, `Toast.svelte`, `Modal.svelte`, `StatCard.svelte`.
  - `Sidebar.svelte`, `Topbar.svelte`, `AppShell.svelte` — the sidebar-app chrome (Search, Memory).
- `src/screens/` — one folder per product:
  - `search/` — Dashboard, Search (+chat), Indexes (bulk ops, modal, toasts), Health, Dialogs gallery. Mock data in `data.js`.
  - `console/` — Command deck service grid.
  - `memory/` — Palaces tree.
  - `agents/` — Chat, Projects, Auth gate, Task failure, Recap (all dark). Shared `AgentsHeader.svelte`.
  - `code/` — trusty-code GUI (flagship) and TUI.

## Conventions

- Dark screens wrap their root in `class="dark"` — the tokens flip; component classes need no changes.
- Screens are fixed 1440×900 frames for review; drop the fixed size on `.screen` for a real app.
- Styling: foundry.css classes first, scoped screen-specific CSS second, everything on tokens (`var(--trusty-*)`). No hard-coded colors except the robot-mark faceplates and ANSI TUI palette.
- All data is static mock data inline (or in `data.js`), ready to be swapped for API calls.
