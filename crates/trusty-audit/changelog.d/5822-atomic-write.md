Changed

- The temp-file-then-rename discipline the state files rely on moved to `workdir::write_atomically`, so `selected-repos.toml` and `audit-targets.toml` share one writer rather than restating it ([#5822](https://github.com/bobmatnyc/trusty-tools/issues/5822))
