Fixed
- The `tcode` CLI's stdio client returns only a true response (`result` or `error`, no `method`) to the awaited request id. A server request that reused the id was returned as the call's result; it is now skipped and logged (#10360).
