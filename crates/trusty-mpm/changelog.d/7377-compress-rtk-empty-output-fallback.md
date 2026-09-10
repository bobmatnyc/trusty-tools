Fixed

- `tm compress` returns the raw text, and warns, when compression emptied a
  non-empty input. The `rtk_binary` path handed back whatever the subprocess
  printed, so an rtk that exited zero having printed nothing turned an
  824-byte `git diff --stat` into 0 bytes and reported it as a successful 100%
  reduction. The backstop added for the native filter chain never covered the
  rtk subprocess.
