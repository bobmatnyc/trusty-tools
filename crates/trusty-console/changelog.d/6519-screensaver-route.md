Added

- A fullscreen screensaver route at `/ui/screensaver` (also reachable at
  `/screensaver`), rendering the machine-status data across the whole viewport
  with no tabs, no theme selector and no scrollbars. It forces the dark palette,
  shows a live clock beside the brand lockup, and rotates every 20 seconds
  between the four host stat cards with service counts and the full per-service
  table. It renders with no user interaction, which is what the coming macOS
  `.saver` bundle needs (#6519, #6520).
- The screensaver survives an unreachable daemon: a failed poll keeps the last
  good snapshot on screen behind an "updated Xm ago" line instead of an error
  box, and the 15s poll doubles after three consecutive failures up to a 60s
  ceiling, resetting on the first success (#6519).
- Optional idle entry, **off by default**: set the `localStorage` key
  `trusty-console-screensaver-idle-minutes` to a positive number of minutes and
  the console navigates to the screensaver after that long without a mouse or
  key event; any input there returns to `/ui`. There is no settings UI for this
  key yet — set it from the browser console. On the screensaver's own URL the
  first click or keypress requests fullscreen (a no-op where the browser refuses
  it) and the next one leaves (#6519).
