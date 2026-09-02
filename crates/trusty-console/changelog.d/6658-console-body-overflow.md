Fixed
- Console pages scroll again. `Screensaver.svelte` clipped `:global(body)`, and
  Vite emits one CSS bundle for the whole SPA, so that rule applied on every tab
  whether the screensaver was mounted or not — later than `App.svelte`'s `body`
  rule at equal specificity, so anything taller than the viewport was
  unreachable. The screensaver still clips itself through `.saver`, which is
  `position: fixed; inset: 0; overflow: hidden` (#6658).
