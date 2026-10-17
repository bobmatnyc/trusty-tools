Fixed
- A build with only the `mcp-schema` feature (`--no-default-features --features mcp-schema`, as trusty-agents uses) is clippy-clean again: the recall access-tracking items whose only caller needs `server` no longer raise `dead_code`, and CI now lints that feature set ([#10013](https://github.com/bobmatnyc/trusty-tools/issues/10013))
