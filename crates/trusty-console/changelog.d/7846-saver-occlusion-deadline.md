Fixed

- macOS screen saver: a load that is in flight while the saver's own window is
  occluded no longer counts as a failed load. WebKit throttles a backgrounded
  `WKWebView`, so the 6 s deadline and the request timeout were measuring the
  throttling rather than the console — 3483 offline transitions against 23
  completed loads in one six-hour window, and 1146 web-view rebuilds whose
  replacements inherited the same occlusion. The deadline now waits while the
  window is off screen, re-asking once a minute, and the attempt is judged with
  a fresh full deadline when the window comes back. An occluded failure of any
  kind no longer feeds the rebuild count, and a window nobody can report on
  waits for three re-asks before falling back to the ordinary deadline
  (#7846).
