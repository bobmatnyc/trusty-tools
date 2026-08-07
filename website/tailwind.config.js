/** @type {import('tailwindcss').Config} */
// Why: this site is the 8th Foundry v2 consumer (#5097, epic #5092). It follows
// the convention the three existing Tailwind consumers already established
// (trusty-agents #3387, trusty-code-gui #3153/#3380, trusty-mpm-gui #3488)
// rather than inventing a fourth: every color resolves to a CSS custom
// property whose VALUE flips between `src/app.css`'s `:root` and `.dark`
// blocks, so a component never hardcodes a hex and never pairs a light class
// with a `dark:` class by hand.
//
// CRITICAL format note: each color is `rgb(var(--color-*) / <alpha-value>)`,
// NOT a bare `var(--color-*)`. Tailwind cannot manipulate a bare `var()`'s
// opacity at build time, so an opacity-modified utility like
// `bg-foundry-primary/10` silently generates NO rule. This trap is documented
// at length in `crates/trusty-agents/ui/tailwind.config.js`; it is the reason
// `app.css` stores space-separated `R G B` triples instead of hex.
//
// What: one `foundry-*` key per canonical `--trusty-*` token this site uses.
// No `light-*` duplicate keys — unlike trusty-agents, this package has no
// pre-Foundry call sites to stay compatible with. `darkMode: 'class'` matches
// `src/lib/theme/index.ts`, which toggles `.dark` on `<html>` (same mechanism as
// `crates/trusty-agents/ui/src/stores/theme.ts`).
// Test: `src/lib/theme/tokens.test.ts` pins every value in `app.css` against
// the canonical `docs/design/UI/design-system/tokens.css`.
export default {
	darkMode: 'class',
	content: ['./src/**/*.{html,js,svelte,ts}'],
	theme: {
		extend: {
			colors: {
				foundry: {
					bg: 'rgb(var(--color-content-bg) / <alpha-value>)',
					card: 'rgb(var(--color-card-bg) / <alpha-value>)',
					raised: 'rgb(var(--color-surface-raised) / <alpha-value>)',
					border: 'rgb(var(--color-border) / <alpha-value>)',
					'border-strong': 'rgb(var(--color-border-strong) / <alpha-value>)',
					text: 'rgb(var(--color-text-primary) / <alpha-value>)',
					secondary: 'rgb(var(--color-text-secondary) / <alpha-value>)',
					muted: 'rgb(var(--color-text-muted) / <alpha-value>)',
					inverse: 'rgb(var(--color-text-inverse) / <alpha-value>)',
					primary: 'rgb(var(--color-primary) / <alpha-value>)',
					'primary-hover': 'rgb(var(--color-primary-hover) / <alpha-value>)',
					success: 'rgb(var(--color-success) / <alpha-value>)',
					warning: 'rgb(var(--color-warning) / <alpha-value>)',
					danger: 'rgb(var(--color-danger) / <alpha-value>)',
					info: 'rgb(var(--color-info) / <alpha-value>)',
					// Dark oxide chassis — the footer strip, which stays dark in
					// BOTH themes (Foundry README, "Flat and honest").
					chassis: 'rgb(var(--color-sidebar-bg) / <alpha-value>)',
					'chassis-text': 'rgb(var(--color-sidebar-text) / <alpha-value>)',
					'chassis-muted': 'rgb(var(--color-sidebar-muted) / <alpha-value>)',
					'chassis-accent': 'rgb(var(--color-sidebar-accent) / <alpha-value>)',
					'chassis-border': 'rgb(var(--color-sidebar-border) / <alpha-value>)'
				}
			},
			fontFamily: {
				// Self-hosted woff2 under `static/fonts/`, declared as @font-face
				// in `app.css`. No CDN: the site must not depend on an external
				// font host. Fallback chains render immediately before load.
				sans: ['"IBM Plex Sans"', '-apple-system', 'BlinkMacSystemFont', 'system-ui', 'sans-serif'],
				display: ['"Chakra Petch"', '"IBM Plex Sans"', 'system-ui', 'sans-serif'],
				mono: ['"IBM Plex Mono"', '"SF Mono"', 'Menlo', 'ui-monospace', 'monospace']
			},
			borderRadius: {
				// Foundry keeps radii at 3/5/8px — no pills.
				sm: '3px',
				DEFAULT: '5px',
				lg: '8px'
			},
			maxWidth: {
				content: '68rem'
			}
		}
	},
	plugins: []
};
