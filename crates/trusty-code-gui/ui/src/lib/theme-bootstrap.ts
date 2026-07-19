// Why: DOC-39 addendum AC-27 (Foundry retheme, issue #3153) — the design
// system activates dark mode via `<html data-theme="dark">` (or a `.dark`
// class), not by keying CSS directly off `prefers-color-scheme`. The old
// #3133 implementation had `app.css` read the media query straight into
// `:root` overrides with no JS layer at all; that no longer matches Foundry's
// activation contract, so this module is the small bridge that reconciles
// them: it is the one place OS appearance turns into the `data-theme`
// attribute Foundry's CSS keys off. System-following stays the *only*
// behavior — no manual toggle, no persisted preference (both explicitly out
// of scope for this pass) — so this is pure translation, not new state.
// What: `applyTheme` sets/removes `data-theme="dark"` on `<html>` from a
// boolean match. `initThemeBootstrap` reads the current
// `(prefers-color-scheme: dark)` match once immediately (so Foundry's
// `[data-theme='dark']` rules are active before `App.svelte` ever mounts —
// the shell's `<body>` renders nothing until Svelte mounts into `#app`, so
// there is no paintable content for a pre-JS flash to show), then subscribes
// the same function to the query's `change` event for the page's lifetime —
// no teardown needed since this runs exactly once per app load, never
// remounted. Guards `matchMedia` behind a feature check: jsdom (this
// project's test environment) does not implement it, and `main.ts` — the
// only caller — never runs under `App.test.ts`'s direct-mount tests, but
// `theme.test.ts` exercises this module directly and needs the guard to be
// meaningful rather than assumed.
// Test: `lib/theme.test.ts` covers `applyTheme`'s attribute set/remove
// behavior and `initThemeBootstrap`'s immediate-apply + change-listener
// wiring against a mocked `matchMedia`.
export function applyTheme(isDark: boolean): void {
  if (isDark) {
    document.documentElement.setAttribute('data-theme', 'dark');
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}

export function initThemeBootstrap(): void {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
  const query = window.matchMedia('(prefers-color-scheme: dark)');
  applyTheme(query.matches);
  query.addEventListener('change', (event) => applyTheme(event.matches));
}
