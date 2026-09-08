Fixed

- The macOS screen saver no longer goes black after its second rotation frame.
  Two behaviours combined. `WallpaperAgent`, which hosts the saver on macOS 26,
  called `startAnimation()` on the already-running view every 20 s to 3.5 min
  with no `stopAnimation()` between, and every one of those calls reset the state
  and reloaded, so the dashboard restarted from its first rotation frame and the
  hourly reload timer was re-armed often enough never to fire. Separately,
  RunningBoard moved the WebContent process to `running-suspended-NotVisible` and
  WebKit ran `freezeAllLayerTrees` → `destroyRenderingResources` →
  `markAllLayersVolatile`, discarding the page's backing store without
  terminating the process — so `webViewWebContentProcessDidTerminate`, the view's
  only exit from the live state, never fired and `draw(_:)` went on deferring to
  a compositor with nothing left to composite. A re-entrant `startAnimation()`
  over a live page now reloads nothing, and while live the view asks the page
  every 10 s what `document.visibilityState` says; anything but `visible`, or no
  answer inside 3 s, puts the dimmed preview up and reloads. It reloads on any of
  four grounds — the saver's own window is not occluded, a visible-then-hidden
  transition was seen, the page stopped answering, or the page has been unhealthy
  for 60 s — so no page can sit suspended behind a live-looking state until the
  hourly reload, and no two recoveries run inside 60 s, so none of the four can
  become a reload treadmill. Every decision names its trigger and its ground in
  the `com.trusty.console.saver` log. `PaintHarness.swift` gains `suspend` and
  `suspend-cold` modes: the first also asserts a page reporting itself visible is
  left alone for 35 s, the second that a page hidden from its first answer still
  recovers.
