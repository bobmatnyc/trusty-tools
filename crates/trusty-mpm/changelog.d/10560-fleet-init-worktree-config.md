Fixed

- `tm fleet init` accepts a directory whose own linked worktree carries the `config.worktree` tm writes, holding only `push.default`; a `config.worktree` with any other key, an include, or a file that is a symlink, unreadable or unparsable still refuses ([#10560](https://github.com/bobmatnyc/trusty-tools/issues/10560))
