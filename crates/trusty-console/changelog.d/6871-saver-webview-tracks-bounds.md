Fixed
- The screen saver's web view is sized from the view's `bounds` on every
  host-driven resize and again on the way into `startAnimation()`, so a host that
  constructs the view at a preview size or 0x0 and supplies the real screen
  afterwards cannot leave the dashboard mis-sized. `autoresizingMask` stays as a
  second line of defence. The saver's `init` line now records the frame it was
  handed and `startAnimation` records the bounds it owned, both at a log level
  `log show` persists, so a fits-the-screen report carries the geometry rather
  than needing a live `log stream`. `PaintHarness.swift` gained a `--frame WxH`
  argument (default 1280x800, unchanged) and a `resize` mode that grows the view
  from a small start frame and asserts the web view and the page's own viewport
  both match the new bounds.
