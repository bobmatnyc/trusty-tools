Added
- The shared memory client (`memory_rpc::call_memory_tool_at_with_timeout`) waits out a `-32007` palace-opening answer: it asks again with a 250 ms to 2 s backoff for up to 30 s (`PALACE_OPENING_WAIT`), then returns the last `-32007` as the error. New `CODE_PALACE_OPENING` and `MemoryRpcError::is_palace_opening` (#11613).
