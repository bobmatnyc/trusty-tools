/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: [
    './index.html',
    './src/**/*.{svelte,js,ts}',
  ],
  theme: {
    extend: {
      colors: {
        // Foundry — Trusty Suite design system, "rust-on-paper" (light) /
        // "Night Shift" (dark) palette. Values ported 1:1 from
        // docs/design/gui/tokens.css — do not introduce new hues here;
        // derive tints in oklch by shifting lightness only if a new tint
        // is ever needed (see docs/design/gui/README.md guardrails).
        foundry: {
          // Dark theme ("Night Shift") values — defaults, since this app
          // renders unprefixed classes for dark:-inverted pairs below.
          bg: '#201612',
          surface: '#2b1c12',
          text: '#f0e7d8',
          muted: '#a58a6b',
          border: '#46311f',
          // Accent brightens one step on dark to hold contrast on oxide
          // paper (tokens.css: --trusty-accent dark = #D97742). Unlike the
          // other bare keys this is genuinely the DARK-theme accent value —
          // see 'light-primary' below for the light-theme counterpart.
          primary: '#d97742',
          teal: '#7ba7c4',
          amber: '#d9a83a',
          // Light theme values (used via `dark:` prefix inversion, same
          // pattern the app already used pre-rebrand).
          'light-bg': '#f5efe7',
          'light-surface': '#fffdf9',
          'light-text': '#2b1c12',
          'light-muted': '#6e5843',
          'light-border': '#d8c9b4',
          'light-primary': '#b7410e',
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
