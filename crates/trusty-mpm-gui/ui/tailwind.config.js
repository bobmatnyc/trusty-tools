/** @type {import('tailwindcss').Config} */
// Why: Foundry v2 retheme (issue #3488, docs/design/UI/design-system/). Every
// `trusty-*`/`status-*` color token used across the components resolves to a
// CSS custom property rather than a hardcoded hex literal — the actual
// light/dark VALUES live in `src/app.css`'s `:root` block (light default)
// and its `[data-theme='dark']` override. This mirrors
// `crates/trusty-code-gui/ui/tailwind.config.js` (the reference
// implementation for this migration) so `--color-*` naming stays 1:1 across
// crates for a future `scripts/check_token_drift.mjs` enforcement flip.
//
// CRITICAL format note: each color is `rgb(var(--color-*) / <alpha-value>)`,
// NOT a bare `var(--color-*)`. This is Tailwind's documented CSS-variable
// pattern (https://tailwindcss.com/docs/customizing-colors#using-css-variables)
// — a bare `var()` reference cannot have its opacity manipulated at build
// time, so `bg-trusty-primary/15`/`hover:bg-trusty-border/60`-style
// modifiers (used throughout every component) silently generate NO rule at
// all with a plain `var()` value. The `/* alpha-value */` placeholder only
// works when the referenced custom property holds space-separated RGB
// channel numbers (`"15 23 42"`, not `"#0f172a"`) — see `app.css`'s
// custom-property declarations.
// What: `darkMode` is intentionally OMITTED — activation is the
// `[data-theme='dark']` attribute `src/stores/theme.ts` manages directly
// (persisted via localStorage, unlike trusty-code-gui's OS-only bootstrap)
// in `app.css`'s custom properties, not a Tailwind `dark:` variant strategy
// (no component needs a `dark:` prefix anymore — every color already
// resolves through a CSS var that itself changes value under
// `[data-theme='dark']`). `status-*` keeps this crate's existing
// session-lifecycle names (running/paused/error/stopped) rather than
// trusty-code-gui's generic ok/warn/neutral names, since components already
// reference them — they resolve to the same underlying `--color-status-*`
// custom properties.
export default {
  content: ['./index.html', './web.html', './src/**/*.{svelte,js,ts}'],
  theme: {
    extend: {
      colors: {
        'trusty-primary': 'rgb(var(--color-primary) / <alpha-value>)',
        'trusty-primary-hover': 'rgb(var(--color-primary-hover) / <alpha-value>)',
        'trusty-surface': 'rgb(var(--color-surface) / <alpha-value>)',
        'trusty-card': 'rgb(var(--color-card) / <alpha-value>)',
        'trusty-raised': 'rgb(var(--color-raised) / <alpha-value>)',
        'trusty-border': 'rgb(var(--color-border) / <alpha-value>)',
        'trusty-border-strong': 'rgb(var(--color-border-strong) / <alpha-value>)',
        'trusty-text': 'rgb(var(--color-text) / <alpha-value>)',
        'trusty-text-secondary': 'rgb(var(--color-text-secondary) / <alpha-value>)',
        'trusty-text-muted': 'rgb(var(--color-text-muted) / <alpha-value>)',
        'trusty-text-inverse': 'rgb(var(--color-text-inverse) / <alpha-value>)',
        // Session status palette — same tokens components already use,
        // now sourced from the shared Foundry status custom properties.
        'status-running': 'rgb(var(--color-status-ok) / <alpha-value>)',
        'status-paused': 'rgb(var(--color-status-warn) / <alpha-value>)',
        'status-error': 'rgb(var(--color-status-error) / <alpha-value>)',
        'status-stopped': 'rgb(var(--color-status-neutral) / <alpha-value>)',
      },
    },
  },
  plugins: [],
};
