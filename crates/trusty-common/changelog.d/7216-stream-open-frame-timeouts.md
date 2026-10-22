Added
- `uds::send_framed_stream_request_with(path, request, open_timeout, frame_timeout)` takes separate budgets for the dial and request write and for each frame read. `send_framed_stream_request` and `send_framed_stream_request_capped` keep their signatures and pass their one figure to both (#7216).
- `uds::test_support::bind_hardened_stub(dir)`, behind the new `uds-test-support` feature, binds a test stub at `dir/stub.sock` with `dir` at `0700` and the socket at `0600`, so `connect_hardened` accepts it. It returns `Result<(UnixListener, PathBuf), UdsSecurityError>` (#7215).
