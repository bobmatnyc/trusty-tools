// Why: Foundry v2 retheme (issue #3487) — tokens.css now ships a full
// `[data-theme='dark']` override block (previously this crate had no dark
// theme at all), but nothing set the attribute it keys off. Foundry
// activates dark via `<html data-theme="dark">`, not by keying CSS
// directly off `prefers-color-scheme` (see trusty-code-gui's
// `lib/theme-bootstrap.ts`, issue #3153 AC-27, which this mirrors) — this
// module is the small bridge that turns OS appearance into that attribute.
// System-following is the only behavior: no manual toggle, no persisted
// preference.
// What: `applyTheme` sets/removes `data-theme="dark"` on `<html>` from a
// boolean match. `initThemeBootstrap` reads the current
// `(prefers-color-scheme: dark)` match once immediately (so the
// `[data-theme='dark']` rules are active before `App.svelte` ever mounts —
// there is no paintable content before that to flash), then subscribes the
// same function to the query's `change` event for the page's lifetime.
// Guards `matchMedia` behind a feature check since jsdom (this project's
// test environment) does not implement it.
// Test: `lib/theme-bootstrap.test.js` covers `applyTheme`'s attribute
// set/remove behavior and `initThemeBootstrap`'s immediate-apply +
// change-listener wiring against a mocked `matchMedia`.
export function applyTheme(isDark) {
  if (isDark) {
    document.documentElement.setAttribute('data-theme', 'dark');
  } else {
    document.documentElement.removeAttribute('data-theme');
  }
}

export function initThemeBootstrap() {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
  const query = window.matchMedia('(prefers-color-scheme: dark)');
  applyTheme(query.matches);
  query.addEventListener('change', (event) => applyTheme(event.matches));
}
