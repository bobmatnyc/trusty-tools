Fixed
- The shutdown flush and the incremental persister no longer overwrite a populated `chunks.json` with an empty in-memory corpus, or with a corpus this indexer never loaded from that file. The write is refused, logged at ERROR with a running refusal count, and returned as an error; the snapshot is left byte-identical (#7920)
