Fixed
- `claude_config::write_json_atomic` keeps an existing file's permission bits. It used to publish every rewrite at the umask default, so a `.claude.json` tightened to 0600 went back to 0644 on the next write (#10041).
