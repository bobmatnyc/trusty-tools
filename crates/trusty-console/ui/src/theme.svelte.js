// Why: Central source of truth for the user's theme choice; decoupled from
//      components so any component can read/set the theme without prop drilling.
//      Named `.svelte.js` so the Svelte 5 compiler processes the module-level
//      `$state` rune (runes are only compiled in .svelte and .svelte.js files).
// What: Exports `theme` (a Svelte 5 $state rune exposed via getter/setter),
//       `applyTheme()` (sets data-theme on <html>), and `initTheme()` (reads
//       localStorage + matchMedia to set initial state and wire up the
//       prefers-color-scheme listener).
// Test: Call initTheme() with localStorage='light' → assert data-theme='light';
//       call with localStorage='system' and matchMedia dark → assert data-theme='dark';
//       call initTheme() with 'light', then set current='system', simulate OS flip →
//       assert data-theme updates (listener re-registered by setter).

const STORAGE_KEY = 'trusty-console-theme';

let _theme = $state('system');
let _mediaListener = null;

// Why: Extracted so both initTheme() and the current setter share exactly one
//      code path for managing the matchMedia listener — no duplication, no drift.
// What: Removes any existing OS-appearance listener, then (if choice='system')
//       registers a fresh one that calls applyTheme('system') on every OS flip.
// Test: Call twice with 'system' → assert only one listener is attached (no
//       duplicates); call with 'light' → assert no listener attached.
function _syncListener(choice) {
  if (_mediaListener) {
    window.matchMedia('(prefers-color-scheme: dark)').removeEventListener('change', _mediaListener);
    _mediaListener = null;
  }
  if (choice === 'system') {
    _mediaListener = () => applyTheme('system');
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', _mediaListener);
  }
}

export const theme = {
  get current() { return _theme; },
  set current(v) {
    _theme = v;
    // Persist to localStorage defensively — a storage exception must not break
    // theme application; we fall back to in-memory state.
    try {
      localStorage.setItem(STORAGE_KEY, v);
    } catch (_) {
      // StorageError or SecurityError in sandboxed/strict-privacy contexts.
    }
    _syncListener(v);
    applyTheme(v);
  }
};

export function applyTheme(choice) {
  const effective = choice === 'system'
    ? (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light')
    : choice;
  document.documentElement.setAttribute('data-theme', effective);
}

export function initTheme() {
  let saved = 'system';
  try {
    saved = localStorage.getItem(STORAGE_KEY) ?? 'system';
  } catch (_) {
    // StorageError or SecurityError — fall back to 'system'.
  }
  _theme = saved;
  _syncListener(saved);
  applyTheme(saved);
}

export function cleanupThemeListener() {
  if (_mediaListener) {
    window.matchMedia('(prefers-color-scheme: dark)').removeEventListener('change', _mediaListener);
    _mediaListener = null;
  }
}
