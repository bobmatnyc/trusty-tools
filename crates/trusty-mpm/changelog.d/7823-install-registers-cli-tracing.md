Fixed

- `tm install` now registers the CLI tracing subscriber, so the warning naming a
  preserved `settings.json.malformed-<stamp>` copy reaches the operator on stderr.
  The command rewrites the tm-owned `settings.json`, and that preservation reports
  only through `tracing`; with no subscriber registered for `Command::Install` the
  one notice that a malformed settings file had been replaced went nowhere
  ([#7823](https://github.com/bobmatnyc/trusty-tools/issues/7823))
