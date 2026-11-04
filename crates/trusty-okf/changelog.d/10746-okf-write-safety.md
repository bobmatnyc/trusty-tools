Changed
- A write refuses to replace a file it did not read: a new concept over an existing file returns `Conflict { reason: "exists" }`, and a concept whose file changed or vanished since it was read returns `stale_write` (#10746).
- A write keeps the replaced file's permissions, so a 0600 concept stays 0600; a new file gets the default create mode (0o666 masked by the umask on Unix).
- `Concept::raw` and `Concept::frontmatter` are now read accessors, the strict check runs on the values that render, and a render that does not read back as those values is refused. `write_concept` is crate-private; `Bundle::put` is the public write.
