Fixed

- Staged reindex promotion no longer renames over a live `index.redb` another process holds open. The commit takes redb's own advisory lock (`flock` on macOS and Linux, the same primitive `redb::Database` takes) on the live file and holds it across the rename; a held, unlockable, or unopenable live file defers the promotion with the staged corpus and the live corpus both intact (#7991).
