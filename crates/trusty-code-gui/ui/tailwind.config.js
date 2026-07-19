/** @type {import('tailwindcss').Config} */
// Why: issue #3133 — the GUI needs light + dark themes that follow the OS
// appearance (`prefers-color-scheme`), not a manual toggle. Every
// `trusty-*`/`status-*` color token used across the components
// (App/StatusBar/HealthPanel/SessionMonitor/SearchTab/CreateSessionForm)
// now resolves to a CSS custom property instead of a hardcoded hex
// literal — the actual light/dark VALUES live in `src/app.css`'s `:root`
// block (light default) and its `@media (prefers-color-scheme: dark)`
// override.
//
// CRITICAL format note: each color is `rgb(var(--color-*) / <alpha-value>)`,
// NOT a bare `var(--color-*)`. This is Tailwind's documented CSS-variable
// pattern (https://tailwindcss.com/docs/customizing-colors#using-css-variables)
// — a bare `var()` reference cannot have its opacity manipulated at build
// time, so `bg-status-ok/15`/`text-trusty-text/60`-style modifiers (used
// throughout every component for hover states, dimmed text, and badges)
// silently generate NO rule at all with a plain `var()` value. The `/*
// alpha-value */` placeholder only works when the referenced custom
// property holds space-separated RGB channel numbers (`"15 23 42"`, not
// `"#0f172a"`) — see `app.css`'s custom-property declarations.
// What: `darkMode` is intentionally OMITTED — no component ever used a
// `dark:` variant (there is no manual toggle to key off), and the previous
// `darkMode: 'class'` config was dead weight: nothing in this codebase ever
// added a `.dark` class to `<html>`, so the old `html:not(.dark)` override
// in `app.css` silently won every render regardless of OS appearance. Theme
// switching is handled entirely by the CSS custom properties + media query
// in `app.css`.
// Test: `lib/theme.test.ts` pins the `rgb(var(--color-*) / <alpha-value>)`
// format and that opacity-modified utilities actually compile (regression
// guard for the silent-no-rule failure mode above).
export default {
  content: ['./index.html', './src/**/*.{svelte,js,ts}'],
  theme: {
    extend: {
      colors: {
        'trusty-primary': 'rgb(var(--color-primary) / <alpha-value>)',
        'trusty-surface': 'rgb(var(--color-surface) / <alpha-value>)',
        'trusty-border': 'rgb(var(--color-border) / <alpha-value>)',
        'trusty-text': 'rgb(var(--color-text) / <alpha-value>)',
        'status-ok': 'rgb(var(--color-status-ok) / <alpha-value>)',
        'status-error': 'rgb(var(--color-status-error) / <alpha-value>)',
        'status-warn': 'rgb(var(--color-status-warn) / <alpha-value>)', // DOC-39 §8: amber = inferred/warming
        'status-neutral': 'rgb(var(--color-status-neutral) / <alpha-value>)', // no-session / never-probed
      },
    },
  },
  plugins: [],
};
