Fixed
- An HNSW graph compaction on Linux no longer leaves a vector copy resident after it ends. The copy now uses 64 MiB pieces, above glibc's mmap threshold cap, so each piece is returned to the OS when freed; with 16 MiB pieces, every compaction after the first kept about one copy (~230 MB at 150K x 384) in the heap (#12007).
