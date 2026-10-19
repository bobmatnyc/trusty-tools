# trusty-events

[![crates.io](https://img.shields.io/crates/v/trusty-events.svg)](https://crates.io/crates/trusty-events)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

The event bus for the trusty-* tools, run as its own daemon. Producers push
`HarnessEvent` frames to it; readers subscribe to the ordered stream.

Design: [ADR-0065](../../docs/adr/0065-trusty-events-process-placement.md) and
[DOC-73](../../docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md) §4.
Tracking: epic [#9074](https://github.com/bobmatnyc/trusty-tools/issues/9074),
[#9075](https://github.com/bobmatnyc/trusty-tools/issues/9075).

## Install and run

```bash
cargo install trusty-events --locked
trusty-events serve
```

`serve` takes `--socket`, `--log-dir`, `--retain-days` (default 7) and
`--ring-capacity` (default 8192). Logs go to stderr; set `RUST_LOG` to change
the level.

Version 0.1.0 has no launchd job, `tctl` member or `tm doctor` row yet; those
ship with [#10042](https://github.com/bobmatnyc/trusty-tools/issues/10042).

## The socket

One Unix socket, `<data dir>/trusty-events/trusty-events.sock`, mode `0600`,
peer uid checked. It never binds TCP (ADR-0032). Clients resolve the path with
`trusty_common::control_bus::events_socket_path()`.

The first line of a connection decides what it is:

- **Producer.** Newline-delimited `HarnessEvent` JSON, the frames
  `trusty_common::control_bus::PushClient` writes. A malformed line is logged
  and skipped; a line over 1 MiB, or 60 s without a complete line, ends that
  connection.
- **Reader.** One JSON-RPC request for `events.subscribe { since_seq, filters }`
  with `"stream": true`, answered as a stream of `SubscribeItem`s. With
  `since_seq` set, the bus replays every retained event after it, from the
  log and then the ring, and then streams live events. A missed event is never
  skipped silently: the stream carries `Gap { skipped }` instead. Resubscribe
  with `since_seq` set to the last `seq` you hold to recover from the log.

The wire contract lives in `trusty_common::control_bus`. This crate's Rust API
is internal to trusty-* and carries no stability promise; no other workspace
crate links it.

## Storage

The bus assigns one `seq` to each event it accepts and appends the event to a
day-rotated NDJSON file under `<data dir>/trusty-events/event_log/`, keeping 7
days. On start it reads the newest file and continues `seq` from there. The
console's old `trusty-console/event_log/` directory is not read.

## Exit status

| Exit | When |
|---|---|
| 0 | SIGTERM or SIGINT: stops assigning `seq`, lets the log writer drain (up to 5 s), removes the socket |
| 1 | Another process serves the socket; the socket cannot be bound; the log directory cannot be opened; accept failed 64 times in a row (about a minute) |
| 2 | Usage error |

### Under launchd

The launchd job (#10042) is expected to use `KeepAlive`. If another
`trusty-events` already serves the socket, for example one started by hand,
the launchd copy exits 1 and launchd starts it again after its throttle
interval (10 s by default). Each attempt costs one process start, one connect
to the live socket, and one stderr line. The attempt binds nothing and opens
no log, because the socket is taken before the log, so it never writes beside
the live instance. When the other instance exits, the next attempt binds and
serves. Stop the hand-started copy to end the loop.

## License

MIT
