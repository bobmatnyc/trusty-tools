Security
- The memory secret filter (`check_secret`) now screens the key of a `KEY=value` token the way it screens a bare token. A random 40-character key in front of a path, such as `<key>=src/main.rs`, was admitted because only the key's characters were checked; it is now refused ([#8589](https://github.com/bobmatnyc/trusty-tools/issues/8589))
