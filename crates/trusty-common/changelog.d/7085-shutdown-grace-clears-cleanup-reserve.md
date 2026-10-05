Fixed
- `parent_death`: the watchdog's graceful-shutdown window is now `shutdown::CLEANUP_RESERVE` plus 5 s (10 s), derived from the reserve rather than a separate 5 s literal. The old window equalled the reserve, so a daemon whose parent died could be force-exited during a full-budget exit flush, before it unlinked its socket (#7085).
