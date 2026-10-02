Fixed

- `~/.trusty-mpm/config.toml` no longer warns `ignoring 1 unrecognised key(s):
  supervisor.session`. `tm fleet init --session` writes that key, so the
  unknown-key report now exempts it. Each remaining unrecognised-key warning
  prints once per file per process, not once per config load (about twenty
  times per `tm doctor` run before).
