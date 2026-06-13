// Why: Central source of truth for the user's theme choice; decoupled from
//      components so any component can read/set the theme without prop drilling.
//      Named `.svelte.js` so the Svelte 5 compiler processes the module-level
//      `$state` rune (runes are only compiled in .svelte and .svelte.js files).
// What: Exports `theme` (a Svelte 5 $state rune exposed via getter/setter),
//       `applyTheme()` (sets data-theme on <html>), and `initTheme()` (reads
//       localStorage + matchMedia to set initial state and wire up the
//       prefers-color-scheme listener).
// Test: Call initTheme() with localStorage='light' → assert data-theme='light';
//       call with localStorage='system' and matchMedia dark → assert data-theme='dark'.

const STORAGE_KEY = 'trusty-console-theme';

let _theme = $state('system');

export const theme = {
  get current() { return _theme; },
  set current(v) {
    _theme = v;
    localStorage.setItem(STORAGE_KEY, v);
    applyTheme(v);
  }
};

let _mediaListener = null;

export function applyTheme(choice) {
  const effective = choice === 'system'
    ? (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : choice;
  document.documentElement.setAttribute('data-theme', effective);
}

export function initTheme() {
  const saved = localStorage.getItem(STORAGE_KEY) ?? 'system';
  _theme = saved;
  applyTheme(saved);

  // Clean up previous listener if any
  if (_mediaListener) {
    window.matchMedia('(prefers-color-scheme: dark)').removeEventListener('change', _mediaListener);
    _mediaListener = null;
  }
  if (saved === 'system') {
    _mediaListener = () => applyTheme('system');
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', _mediaListener);
  }
}

export function cleanupThemeListener() {
  if (_mediaListener) {
    window.matchMedia('(prefers-color-scheme: dark)').removeEventListener('change', _mediaListener);
    _mediaListener = null;
  }
}
