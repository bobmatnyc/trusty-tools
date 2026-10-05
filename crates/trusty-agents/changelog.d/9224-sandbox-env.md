Fixed

- tagent startup loads neither the cwd `.env` nor the self-project `.env.local`
  when `TRUSTY_SANDBOX` is exactly `1`
  ([#9224](https://github.com/bobmatnyc/trusty-tools/issues/9224)). These two
  loads bypassed trusty-common's shared loader, so a session started by a
  sandboxed tm daemon still read developer credentials. Any other value,
  including empty, `0` and `true`, still loads both files.
- tagent again resolves its self-project after loading the cwd `.env`, so a
  `TAGENT_PROJECT_DIR` hint set there picks which `.env.local` loads; and the
  installed `tagent` wrapper (`scripts/tagent-wrapper.sh`) exports nothing
  from `.env.local` under `TRUSTY_SANDBOX=1` and never lets that file set the
  flag ([#9224](https://github.com/bobmatnyc/trusty-tools/issues/9224)).
