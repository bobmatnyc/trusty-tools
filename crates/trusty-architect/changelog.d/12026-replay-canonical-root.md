Fixed

- `trusty-architect-replay` no longer refuses a Python script's own reads as outside the sandbox when its directory sits behind a symlink, as macOS temp dirs do (`/var` is `/private/var`). The open probe now compares canonical paths with a canonical sandbox root, script dir and `sys.path`, and a script dir that cannot be resolved is an error (Refs [#12026](https://github.com/bobmatnyc/trusty-tools/issues/12026))
