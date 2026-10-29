Fixed
- The `tcode` CLI's stdio client rejects a daemon stdout line over 16 MiB with an error instead of buffering it whole, then resumes at the next newline; reads are now cancel-safe under the call timeout. The warn log for a frame that reuses the awaited id truncates the child-supplied method (#10360).
