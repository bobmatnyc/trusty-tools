Fixed
- A launchd-started `trusty-search start --foreground` no longer stands by forever behind a lock holder that predates the installed binary. It waits at most 10 minutes (so launchd retries at that rate, not every 30 s), then exits 1 with a message that the holder predates the installed binary and how to replace it (#10358).
