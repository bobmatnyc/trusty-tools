Fixed
- `bind_singleton_hardened` now releases its bind lock explicitly, so a child forked during the bind no longer leaves the next binder refused with `BindInProgress` (#10661).
