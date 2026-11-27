Fixed

- Two `cargo test -p trusty-review` hangs no longer occur: a test that read from an inherited stdin, and a deadlock at the dedup barrier (Refs [#12040](https://github.com/bobmatnyc/trusty-tools/issues/12040))
