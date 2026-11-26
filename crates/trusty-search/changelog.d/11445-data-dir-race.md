Fixed
- Test suite only: corpus-attaching unit tests now join the `serial_test` group, so they no longer write `index-identity/` into the data dir a serial test points `TRUSTY_DATA_DIR` at; the env ratchet enforces it (#11445).
