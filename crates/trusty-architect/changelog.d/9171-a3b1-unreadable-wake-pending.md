Fixed

- `poller::dispatch_wake` reads a stored `wake_pending` that is not a non-negative integer (a string, a fraction, a negative) as 1 instead of 0, so the wake still runs and only a result starting `sent` clears the count ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
