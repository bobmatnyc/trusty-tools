Fixed
- The screensaver picks its frame from the wall clock rather than from time
  since the page mounted, and its rotation timer waits out the remainder of the
  current frame before its first transition. System Settings' screensaver
  Preview rebuilds the WKWebView every few seconds, so the old mount-relative
  rotation restarted at frame 0 on every rebuild and the per-service CPU and
  memory frame — 20 s in — never appeared there at all
  ([#6828](https://github.com/bobmatnyc/trusty-tools/issues/6828)).
