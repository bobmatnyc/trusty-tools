# trusty-kb

Deterministic Markdown knowledge-base maintenance exposed as an MCP stdio
server. It owns the per-assistant knowledge-tree structure, YAML frontmatter,
validation, and in-place conversion of existing document trees.

`trusty-kb` is an internal workspace package (`publish = false`). Build or run
it from this repository:

```bash
cargo build -p trusty-kb
cargo run -p trusty-kb -- serve --stdio
```

By default, one server manages assistant trees under
`~/.trusty-agents/knowledge` and uses the `bob-kb` subtree as its default root.
Override those locations with flags or environment variables:

```bash
cargo run -p trusty-kb -- serve --stdio \
  --knowledge-dir /path/to/knowledge \
  --root /path/to/knowledge/assistant-kb
```

| Flag | Environment variable | Purpose |
|---|---|---|
| `--knowledge-dir PATH` | `KB_KNOWLEDGE_DIR` | Directory containing one knowledge tree per assistant |
| `--root PATH` | `KB_ROOT` | Default tree for calls that do not select another root |

## MCP tools

The server exposes `kb_status`, `kb_list_trees`, `kb_put_entity`,
`kb_get_entity`, `kb_list`, `kb_ensure_structure`, `kb_validate`, and
`kb_convert_tree`. Tool schemas are defined in [`src/tooldefs.rs`](src/tooldefs.rs),
and dispatch behavior lives in [`src/mcp.rs`](src/mcp.rs).

## Development

```bash
cargo check -p trusty-kb
cargo test -p trusty-kb --no-fail-fast
cargo clippy -p trusty-kb --all-targets --all-features -- -D warnings
```

The workspace is licensed under the [MIT License](../../LICENSE).
