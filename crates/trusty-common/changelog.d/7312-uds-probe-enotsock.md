Fixed

- `uds::bind_singleton_hardened` refuses a socket path holding anything that is
  not a socket, instead of unlinking it and binding over it. The takeover
  decision used to rest on the connect probe alone, and the two platforms
  disagree about which errno a connect to a non-socket returns: macOS/BSD
  answers `ENOTSOCK`, which classified as `Inconclusive` and refused, while
  Linux's `unix_find_other` sets `-ECONNREFUSED` before its `S_ISSOCK` test,
  which classified as `NotServing` and licensed the unlink. So on Linux a daemon
  pointed at a path holding an ordinary file deleted that file and came up
  serving. The file type is now read with `lstat` and outranks the probe, and
  the refusal is its own `UdsSecurityError::NotASocketFile` naming what is
  actually there rather than the borrowed `AlreadyServing`
  ([#7312](https://github.com/bobmatnyc/trusty-tools/issues/7312),
  [#7219](https://github.com/bobmatnyc/trusty-tools/pull/7219))
