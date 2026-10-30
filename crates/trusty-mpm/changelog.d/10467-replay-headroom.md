Fixed

- A start replays at most half its push buffer from the spool, and sends the replay to trusty-events at once instead of one flush interval later. Hooks that arrive before that first flush no longer evict replayed events, so a replayed spool file retires and the files deferred behind it replay on the next start (Refs [#10467](https://github.com/bobmatnyc/trusty-tools/issues/10467))
- The log line for spool files left on disk (held by another process, or deferred past the replay budget) is now a warning
