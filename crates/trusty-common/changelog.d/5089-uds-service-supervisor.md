Added

- `uds::supervisor::UdsServiceSupervisor`, behind the new `uds-supervisor`
  feature: the on-demand spawn supervisor generalised out of `trusty-memory`'s
  `Bm25Supervisor`, so `trusty-console` can start `trusty-review` /
  `trusty-analyze` at webhook-delivery time without either being resident
  (ADR-0034 §1, milestone `tm 1.3.5` criterion (c)). It carries the whole state
  machine that crate had already hardened: spawn-gate serialisation against
  double-spawn and against a fan-out that satisfies the cap per-caller,
  adoption of a socket another process bound, an LRU cap on live children, an
  RSS ceiling compared against a real measurement, exponential-backoff socket
  probing, `kill_on_drop`, and SIGTERM→SIGKILL with a patience window (#5089)
- Liveness is decided by the socket, never by `try_wait()`. `SocketVerdict` is
  three-state and only ENOENT / ECONNREFUSED mean `NotServing`; a timeout or any
  other errno is `Inconclusive` and leaves the child alone, so load cannot turn
  into a respawn storm. A child evicted while still alive goes onto a `doomed`
  queue drained under the spawn gate — SIGTERM plus socket unlink, in that order
  — rather than being dropped onto `kill_on_drop`'s SIGKILL, because the doomed
  child unlinks the shared socket path as the last step of its own shutdown and
  a concurrent termination could land that unlink after the replacement bound
  the same path (#5085, #5119, #5089)
- `ServiceTimeouts` makes the spawn-probe budget, the child's own shutdown-flush
  budget and the SIGTERM patience per-service values rather than constants. Its
  constructor is a `const fn` whose assert enforces `sigterm_patience >
  shutdown_flush`, so a service that declares its timeouts as a `const` gets the
  same compile-time failure `bm25_supervisor.rs`'s `const _: () = assert!(…)`
  gave — now bound to the value actually passed to the supervisor rather than to
  a free-standing constant (#5089)
- Socket adoption is verified, not assumed: a socket that answers but fails
  `verify_socket_for_connect` is refused with `SupervisorError::UntrustedSocket`
  instead of being adopted silently or falling through to a spawn that dies on
  EADDRINUSE. This is the one path where the target's own `bind_hardened` never
  runs, which ADR-0034 §3 makes the permission bits load-bearing for (#5099,
  #5089)
- Every new public enum and struct is `#[non_exhaustive]` while the crate is
  unpublished at 0.30.0 against a released 0.28.1 (#5089)
- `ServiceTimeouts::try_new` is the fallible sibling of the `const fn`
  constructor, for a service deriving its timeouts from config rather than
  declaring them as a `const`. `#[non_exhaustive]` left no other way to build
  the value, so without it the runtime case could only panic. The type also now
  documents the sourcing rule the assert cannot check: `shutdown_flush` must be
  the supervised binary's own flush constant, imported, not a literal that
  happens to match it today (#5089)
