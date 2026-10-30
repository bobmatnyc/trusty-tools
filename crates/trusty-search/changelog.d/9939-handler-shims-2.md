Removed

- Test-only HTTP handler shims for the logs-tail, admin-stop, index-config get/patch and quantize routes; their tests now call the `*_report` cores the socket serves (#9939, no runtime change).
