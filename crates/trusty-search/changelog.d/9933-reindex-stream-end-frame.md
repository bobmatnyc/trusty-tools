Fixed
- A live `search.index.reindex.stream` now ends with an `end` frame after its terminal event. It used to stay open after `complete`, so the CLI hung up mid-stream and every live reindex logged `uds rpc connection failed ... the stream's peer closed the socket` (#9933).
- A reindex that panics or is cancelled now records its fatal `error` event in the replay buffer, under the same lock as its `Failed` status, so a stream opened after the failure replays the error and a live stream never ends without it (#9933).
