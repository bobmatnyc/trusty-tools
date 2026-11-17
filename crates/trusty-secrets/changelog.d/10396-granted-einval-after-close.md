Fixed
- The granted client no longer fails a resolve with `EINVAL` on macOS when the server closes the socket while answer bytes are still buffered (#10396).
