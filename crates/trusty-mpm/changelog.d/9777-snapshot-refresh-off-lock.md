Fixed
- The daemon channel policy snapshot refreshes on its own thread. A check never waits on git or the policy load, at most one refresh runs at a time, and a refresh slower than 5 s no longer starts another: the next one is due 5 s after the last one finished. While the first load runs, a check denies with `NeverLoaded` (#9777).
