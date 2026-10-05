Fixed

- tagent startup loads neither the cwd `.env` nor the self-project `.env.local`
  when `TRUSTY_SANDBOX` is exactly `1`
  ([#9224](https://github.com/bobmatnyc/trusty-tools/issues/9224)). These two
  loads bypassed trusty-common's shared loader, so a session started by a
  sandboxed tm daemon still read developer credentials. Any other value,
  including empty, `0` and `true`, still loads both files.
