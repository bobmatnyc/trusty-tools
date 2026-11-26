Fixed

- A session started in a linked git worktree now registers its main checkout as a project, so `tm secrets assign` no longer answers "project not registered" for it. A worktree whose main checkout cannot be resolved (a bare repository, a missing common dir, a deleted main checkout) registers nothing and logs a warning (#11217).
