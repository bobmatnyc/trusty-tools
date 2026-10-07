Fixed
- Tests that read the data dir now run under `#[serial_test::parallel]`, so they never overlap a test that points `TRUSTY_DATA_DIR` at a scratch or invalid path. Two `create_index` tests intermittently answered `500` under a shared-process `cargo test`; a non-`200` there now prints the response body (#9233).
