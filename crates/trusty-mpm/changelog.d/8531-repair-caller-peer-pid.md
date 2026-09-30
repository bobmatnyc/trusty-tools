Fixed

- `tm repair delegation` no longer trusts a caller-written session id. The
  owning session's right to clear its own live record is granted only over
  the daemon socket, when the kernel-reported peer pid runs under that
  session's own `claude` process. The `x-tm-caller-session` header and the
  socket `caller_session` param are ignored, so a sibling session can no
  longer impersonate the owner. A caller whose process, ancestry or owner
  process cannot be read is refused (#8531).
- `tm repair delegation` now always reaches the daemon over its socket. The
  HTTP repair routes still end records whose owner is gone, stale or forced,
  but never establish an owner (#8531).
