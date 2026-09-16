Fixed

- The `chunks.json` overwrite guard no longer refuses every save from a store-less, non-quarantined indexer whose target file is unreadable. The unreadable bytes are renamed to a `.corrupt-<millis>` sidecar and the write lands; a write-quarantined or corpus-detached writer is still refused, and nothing is ever destroyed (#7980).
