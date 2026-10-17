Fixed
- `palace_rename` waits until the old palace's `kg.redb` and vector redb files are closed before it moves the directory. On a busy runtime the renamed palace could reopen on a read-only snapshot, and the next write through either id failed with "HTTP daemon holds the write lock" although no daemon was running ([#9945](https://github.com/bobmatnyc/trusty-tools/issues/9945))
