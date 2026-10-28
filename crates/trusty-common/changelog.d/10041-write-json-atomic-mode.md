Fixed
- `claude_config::write_json_atomic` keeps an existing file's permission bits, creates a new file at 0600, and creates its staging file at 0600 before any byte is written. It used to publish every rewrite at the umask default, so a `.claude.json` tightened to 0600 went back to 0644 on the next write (#10041).
