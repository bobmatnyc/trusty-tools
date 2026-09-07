Changed
- The analyze dashboard no longer opens an `EventSource` on `/sse`, and its
  `sse` status pill is gone. #6287 deleted that route and the `AnalyzerEvent`
  broadcast behind it without putting a streaming RPC method in their place, so
  the subscription reconnected forever against nothing. The ten-second
  `/health` poll is the liveness signal that remains
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
