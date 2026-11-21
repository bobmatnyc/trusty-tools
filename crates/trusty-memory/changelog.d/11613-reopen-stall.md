Fixed
- The idle-evict sweep keeps a palace whose last open took 1 s or more, so the first call after an idle period no longer re-opens it past the client's 5 s exchange and fails with a broken pipe (#11613).
