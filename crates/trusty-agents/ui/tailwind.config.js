/** @type {import('tailwindcss').Config} */
// Why: Foundry v2 token layer (issue #3387, docs/design/UI/design-system/).
// Every `foundry-*` color used across the components now resolves to a CSS
// custom property (`app.css`'s `:root` / `.dark` blocks) rather than a
// hardcoded hex literal — previously this file baked 15 hex values directly
// into `theme.extend.colors.foundry`, the root cause of the crate having no
// CSS-variable indirection at all (issue #3387's audit finding B1/B3/D4).
//
// CRITICAL format note: each color is `rgb(var(--color-*) / <alpha-value>)`,
// NOT a bare `var(--color-*)` — see `trusty-code-gui/ui/tailwind.config.js`
// for the same established pattern. A bare `var()` reference cannot have its
// opacity manipulated at build time, so the `bg-foundry-primary/10`-style
// modifiers used throughout every component (hover states, dimmed text,
// badges) would silently generate NO rule with a plain `var()` value.
//
// What: the dark-named key (`primary`) and the light-named key
// (`light-primary`) intentionally map to the SAME `--color-*` variable — the
// variable itself already changes value between `:root` (light) and `.dark`
// (dark), so the app's existing `text-foundry-light-X dark:text-foundry-X`
// pairing throughout every component keeps working with zero component
// edits: whichever class wins Tailwind's dark: cascade, both ultimately read
// the same token and render the same color for the active theme. `teal` and
// `amber` now resolve to `--color-info`/`--color-warning` — previously
// dark-only literals with no light-mode counterpart (audit finding D4) — so
// they correctly invert in light mode as a side effect of this change.
// `darkMode: 'class'` is unchanged: `stores/theme.ts` toggles the `.dark`
// class on `<html>`, which both this config and `app.css` key off.
// Test: manual — see `app.css`'s header comment for the verification note.
export default {
  darkMode: 'class',
  content: [
    './index.html',
    './src/**/*.{svelte,js,ts}',
  ],
  theme: {
    extend: {
      colors: {
        foundry: {
          // Dark-named keys — kept for backward compatibility with every
          // existing `dark:text-foundry-X` call site; resolve through the
          // same variable as their light-named counterpart below.
          bg: 'rgb(var(--color-content-bg) / <alpha-value>)',
          surface: 'rgb(var(--color-card-bg) / <alpha-value>)',
          text: 'rgb(var(--color-text-primary) / <alpha-value>)',
          muted: 'rgb(var(--color-text-muted) / <alpha-value>)',
          border: 'rgb(var(--color-border) / <alpha-value>)',
          primary: 'rgb(var(--color-primary) / <alpha-value>)',
          teal: 'rgb(var(--color-info) / <alpha-value>)',
          amber: 'rgb(var(--color-warning) / <alpha-value>)',
          'sidebar-accent': 'rgb(var(--color-sidebar-accent) / <alpha-value>)',
          // Light-named keys (used via `dark:` prefix inversion) — same
          // variables as above; the variable's own value differs per theme.
          'light-bg': 'rgb(var(--color-content-bg) / <alpha-value>)',
          'light-surface': 'rgb(var(--color-card-bg) / <alpha-value>)',
          'light-text': 'rgb(var(--color-text-primary) / <alpha-value>)',
          'light-muted': 'rgb(var(--color-text-muted) / <alpha-value>)',
          'light-border': 'rgb(var(--color-border) / <alpha-value>)',
          'light-primary': 'rgb(var(--color-primary) / <alpha-value>)',
        },
      },
      fontFamily: {
        // Self-hosted from public/fonts/ via @font-face rules in index.html
        // (Foundry README install step) — no external CDN dependency so the
        // desktop app renders correctly fully offline. Fallback chains keep
        // text rendering immediately with system fonts before the woff2
        // loads.
        sans: ['"IBM Plex Sans"', '-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'system-ui', 'sans-serif'],
        display: ['"Chakra Petch"', '"IBM Plex Sans"', 'system-ui', 'sans-serif'],
        mono: ['"IBM Plex Mono"', '"SF Mono"', 'Monaco', '"Cascadia Code"', 'Consolas', 'ui-monospace', 'monospace'],
      },
    },
  },
  plugins: [],
};
