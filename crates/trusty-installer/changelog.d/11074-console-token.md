Fixed
- `tctl` health probes of trusty-console send the console token when `TRUSTY_CONSOLE_TOKEN_FILE` is set, so a console with its token on reads as healthy instead of as an HTTP `401` (#11074).
