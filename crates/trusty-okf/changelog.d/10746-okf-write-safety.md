Changed
- A write refuses to replace a file it did not read: a new concept over an existing file returns `Conflict { reason: "exists" }`, and a concept whose file changed or vanished since it was read returns `stale_write` (#10746).
- Every file a bundle write creates or replaces (concepts, `index.md`, `log.md`, verification records, moved files) is mode 0600 on Unix, whatever the umask or the prior file's mode, because a bundle can hold secrets (#10746).
- `Concept::raw` and `Concept::frontmatter` are now read accessors, the strict check runs on the values that render, and a render that does not read back as those values is refused. `write_concept` is crate-private; `Bundle::put` is the public write.
