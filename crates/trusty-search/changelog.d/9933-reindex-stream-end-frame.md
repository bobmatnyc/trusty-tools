Fixed
- A live `search.index.reindex.stream` now ends with an `end` frame after its terminal event. It used to stay open after `complete`, so the CLI hung up mid-stream and every live reindex logged `uds rpc connection failed ... the stream's peer closed the socket` (#9933).
