Documentation

- `service::rpc::streams`' module doc described the pre-#7217 transport, where
  the server learned of a disconnect only at its next write and an abandoned
  reindex producer could stay parked for a stall's duration or until the
  progress record's 60 s garbage collection. `write_stream` now reads the peer
  off the socket, so a closed client ends the handler within one poll interval
  and the dropped receiver reaches every producer through its
  `Sender::closed()` arm. `REINDEX_PROGRESS_TTL_SECS` still expires the progress
  record to bound daemon memory (#75); it is no longer what frees the producer.
  No behaviour change here — doc only.
  ([#7217](https://github.com/bobmatnyc/trusty-tools/issues/7217))
