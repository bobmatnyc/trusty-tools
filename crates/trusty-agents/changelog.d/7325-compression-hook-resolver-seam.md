Fixed

- `tool_loop`'s compression hook takes the `rtk` resolver through `compress_success_result_using`, so the test that asserts the recorded `compression_path` forces the native chain instead of assuming no host has `rtk` installed ([#7325](https://github.com/bobmatnyc/trusty-tools/issues/7325)).
