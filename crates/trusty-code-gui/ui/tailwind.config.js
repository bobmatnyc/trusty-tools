/** @type {import('tailwindcss').Config} */
// Why: Foundry retheme (issue #3153, docs/design/UI/design-system/). Every
// `trusty-*`/`status-*` color token used across the components
// (App/StatusBar/HealthPanel/SessionMonitor/SearchTab/CreateSessionForm)
// resolves to a CSS custom property rather than a hardcoded hex literal —
// the actual light/dark VALUES live in `src/app.css`'s `:root` block (light
// default) and its `[data-theme='dark']` override. This keeps the
// rgb-triple/<alpha-value> Tailwind plumbing issue #3133 established (see
// the format note below); only the token set and the actual color values
// changed, from the placeholder slate/indigo palette to Foundry's
// rust-on-paper (light) / Night Shift (dark) palette
// (docs/design/UI/design-system/tokens.css).
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
// What: `darkMode` is intentionally OMITTED — activation is the
// `[data-theme='dark']` attribute `lib/theme-bootstrap.ts` manages directly
// in `app.css`'s custom properties, not a Tailwind `dark:` variant strategy
// (no component ever needs a `dark:` prefix — every color already resolves
// through a CSS var that itself changes value under `[data-theme='dark']`).
// `borderRadius`/`borderWidth` extend (not replace) Tailwind's defaults with
// the Foundry scale — 3/5/8px radii, 1px dividers / 1.5px containers
// (docs/design/UI/design-system/README.md guardrails) — and `fontFamily`
// wires the three self-hosted Foundry faces (`index.html`'s `@font-face`
// rules) with system-font fallbacks first so text renders immediately
// before the woff2 loads.
// Test: `lib/theme.test.ts` pins the `rgb(var(--color-*) / <alpha-value>)`
// format and that opacity-modified utilities actually compile (regression
// guard for the silent-no-rule failure mode above), plus the Foundry token
// values/activation mechanism.
export default {
  content: ['./index.html', './src/**/*.{svelte,js,ts}'],
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
        'status-ok': 'rgb(var(--color-status-ok) / <alpha-value>)',
        'status-error': 'rgb(var(--color-status-error) / <alpha-value>)',
        'status-warn': 'rgb(var(--color-status-warn) / <alpha-value>)', // DOC-39 §8: amber = inferred/warming
        'status-neutral': 'rgb(var(--color-status-neutral) / <alpha-value>)', // no-session / never-probed
        // Issue #3153 shell rebuild: the workstream rail (`.wsrail`) is a
        // fixed dark chassis in BOTH app themes (docs/design/UI/design-system/
        // tokens.css's `--trusty-sidebar-*` set, PM ruling "Foundry wins on
        // visuals" over the PDF's fixed-constant proposal) — `sidebar-bg`
        // still tracks the app theme (a touch darker in Night Shift) even
        // though the chassis itself never goes light.
        'trusty-sidebar-bg': 'rgb(var(--color-sidebar-bg) / <alpha-value>)',
        'trusty-sidebar-text': 'rgb(var(--color-sidebar-text) / <alpha-value>)',
        'trusty-sidebar-muted': 'rgb(var(--color-sidebar-muted) / <alpha-value>)',
        'trusty-sidebar-border': 'rgb(var(--color-sidebar-border) / <alpha-value>)',
        'trusty-sidebar-active': 'rgb(var(--color-sidebar-active) / <alpha-value>)',
      },
      fontFamily: {
        sans: ['"IBM Plex Sans"', '-apple-system', 'BlinkMacSystemFont', 'system-ui', 'sans-serif'],
        display: ['"Chakra Petch"', '"IBM Plex Sans"', 'system-ui', 'sans-serif'],
        mono: ['"IBM Plex Mono"', 'ui-monospace', 'Menlo', 'Consolas', 'monospace'],
      },
      borderRadius: {
        sm: '3px',
        DEFAULT: '5px',
        md: '5px',
        lg: '8px',
      },
      borderWidth: {
        DEFAULT: '1px',
        1.5: '1.5px',
      },
    },
  },
  plugins: [],
};
