Fixed

- `tm hook --pm-guard` no longer assumes a `cd` always ran. A `cd` that the shell may skip (`false && cd <wt>;`), that may fail (`cd <removed-worktree>;`), that runs in a subshell (`cd <wt> | …`, `cd <wt> &`), or that sits in a here-document handed to `bash`/`sh` now leaves the old directory as a candidate. A `git commit` or a destructive git command after it is judged against both directories, so it is refused when it could land in a main checkout. A `cd` that surely ran, through `&&` or into a directory that exists, still moves the directory. (#9146)
