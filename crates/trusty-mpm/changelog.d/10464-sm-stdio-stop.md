Fixed
- `tm sm serve --stdio` now logs the final count of action events lost after the stop settled the buffer when stdin ends, as it already did on SIGTERM. A setup error after the start no longer loses the events the start replayed from the spool: they are flushed or written back to the spool first. The daemon logs the same final count when its socket stops serving with an error (#10464).
