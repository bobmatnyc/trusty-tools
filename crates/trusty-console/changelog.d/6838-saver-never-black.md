Fixed
- `TrustyConsole.saver` no longer shows a black screen while the console is
  unreachable. A load now carries a 5 s timeout and a watchdog, so a daemon that
  has bound its port mid-restart but cannot yet answer drops to the fallback in
  seconds instead of holding the view dark for `URLRequest`'s 60 s default; the
  view overrides `animateOneFrame()` so the fallback is repainted every second
  while the live page is off screen, rather than depending on a navigation
  callback that may never come; and retries run every 5 s for the first three
  minutes of an outage and every 30 s after, picking up the live page as soon as
  one succeeds without restarting the saver
  ([#6838](https://github.com/bobmatnyc/trusty-tools/issues/6838)).
