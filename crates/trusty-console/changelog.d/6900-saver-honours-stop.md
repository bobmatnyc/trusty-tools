Fixed
- `TrustyConsole.saver` now honours `loginwindow`'s stop request immediately, so
  Touch ID is offered at unlock instead of waiting on a saver that has not
  stopped. `stopAnimation()` used to set the same `.offline` state the retry
  timer reads as "keep trying", leave the navigation delegate attached, and
  navigate to `about:blank` over an in-flight console load — WebKit reported that
  cancellation, the offline handler re-armed the retry timer the stop had just
  invalidated, and the saver went on loading the console for four minutes after
  being told to stop. The stop now enters a terminal state that only
  `startAnimation()` leaves, cancels the in-flight load, and detaches the
  delegate before navigating away, so no late callback can restart the loop
  ([#6900](https://github.com/bobmatnyc/trusty-tools/issues/6900)).
