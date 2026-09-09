Fixed
- `uds::server::write_stream` ends when its peer closes the socket, instead of
  waiting on the producer channel alone. A client that hung up on a quiet stream
  used to leave the connection handler parked, so `serve_until`'s shutdown drain
  spent its whole budget before returning. The departure is reported as the
  `BrokenPipe` write failure it is, and a client that merely half-closes its
  write side — which every streaming client does once its request frame is out —
  is not mistaken for one that left. `write_stream` now takes `&mut UnixStream`
  rather than any `AsyncWrite`, because reading the peer's state needs the socket
  ([#7217](https://github.com/bobmatnyc/trusty-tools/issues/7217)).
