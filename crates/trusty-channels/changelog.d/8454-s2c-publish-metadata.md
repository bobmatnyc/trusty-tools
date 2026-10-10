Changed

- `slack-mcp` sends with no route check, so it now builds only with the new non-default `unrouted-slack-mcp` feature: `cargo install trusty-channels --features unrouted-slack-mcp`. A plain `cargo install trusty-channels` installs `gchat-mcp` and `telegram-mcp` only. The library API is unchanged (refs [#8454](https://github.com/bobmatnyc/trusty-tools/issues/8454))
- The manifest carries the crates.io metadata for a first 0.1.x publish (rust-version, repository, readme, keywords, categories, homepage), and the package ships `src/`, `README.md` and `CHANGELOG.md` only. The README describes what 0.1.x does and does not enforce (refs [#8454](https://github.com/bobmatnyc/trusty-tools/issues/8454))
