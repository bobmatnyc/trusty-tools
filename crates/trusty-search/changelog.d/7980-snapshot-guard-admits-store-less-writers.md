Fixed

- The `chunks.json` overwrite guard no longer refuses every save from a store-less, non-quarantined indexer whose target file is unreadable. The unreadable bytes are renamed to a `.corrupt` sidecar (numbered `.corrupt.1`, `.corrupt.2`, … on collision, so a later occurrence never clobbers an earlier one) and the write lands; a write-quarantined or corpus-detached writer is still refused, a preservation that cannot run falls back to the refusal, and nothing is ever destroyed (#7980).
