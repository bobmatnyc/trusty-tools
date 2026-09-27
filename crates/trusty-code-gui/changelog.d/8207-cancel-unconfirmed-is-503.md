Fixed

- a cancel the daemon has not confirmed yet reaches the webview as HTTP 503, not 500 ([#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207))
  - the bridge's status table had no arm for the daemon's `-32010 cancel_unconfirmed`, so "the run has not stopped yet" was indistinguishable from a daemon fault
