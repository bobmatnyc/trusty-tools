Fixed
- The screen saver rebuilds its `WKWebView` after three consecutive failed
  loads, instead of reloading forever into a WebContent process the OS has
  frozen. A reload lands in the same process, so a page macOS has marked
  NotVisible comes back hidden and the recovery reloads it again: the owner's
  saver ran that loop for three days, showing the bundled offline preview while
  `/ui/screensaver` answered in under a millisecond throughout. The rebuild is
  decided on the failure COUNT alone, never on what the page reports about its
  own visibility — a page reporting itself hidden inside a saver the host is
  animating is not evidence of occlusion. It logs the count and the reason to
  `com.trusty.console.saver` and is rate-limited to one a minute, widening to
  one per ten minutes once the console has been down longer than the fast-retry
  window, so a genuinely dead daemon does not turn into process churn
  ([#7606](https://github.com/bobmatnyc/trusty-tools/issues/7606)).
- A retry no longer cancels the load it is retrying. The retry delay was a flat
  5 s against a 6 s load deadline, so every retry issued a `load` over an
  attempt WebKit had not finished with; WebKit reported that supersession as
  `NSURLErrorCancelled` (-999), the view read its own cancellation as a fresh
  network failure and armed another 5 s retry, and the loop fed itself — 139 of
  those in two hours of the owner's log. The delay is now derived from the
  deadline rather than chosen, and the view cancels any attempt it abandons and
  recognises the cancellation that comes back, so a -999 it counts can only have
  come from outside ([#7606](https://github.com/bobmatnyc/trusty-tools/issues/7606)).
