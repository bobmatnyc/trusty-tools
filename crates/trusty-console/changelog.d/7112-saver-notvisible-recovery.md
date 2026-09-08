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
  answer inside 3 s, puts the dimmed preview up and reloads. A `hidden` reading
  acts only on a page that has previously answered `visible`, and no two
  recoveries run inside 60 s, so a host that never shows the page cannot turn the
  recovery into a reload treadmill. Both attempted recoveries name their trigger
  in the `com.trusty.console.saver` log. `PaintHarness.swift` gains a `suspend`
  mode covering both.
