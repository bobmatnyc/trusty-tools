Fixed
- A one-shot exec grant is spent only after every `secrets.resolve` check passed and the allow record was written. An unreadable caller ancestry, an audit failure or a backend failure no longer uses it up (#9621).
- Grants whose child process has exited are dropped at the next mint or idle check. They no longer keep the server from its idle exit or hold one of the 1024 grant slots, and their tokens are refused (#9621).
- An npm-installed Claude Code (`node .../@anthropic-ai/claude-code/cli.js`) counts as an agent ancestor: a `node` process is judged on its argv, read from `/proc/<pid>/cmdline` on Linux and `KERN_PROCARGS2` on macOS. A plain `node` does not count. A `node` whose argv cannot be read refuses (#9621).
- `secrets.set_agents_may_use` writes its allow record under the key's index lock, after the row check. Records and flag changes for one key land in the same order, and an unindexed key leaves one `not_found` deny with no allow record (#9621).
- The `audit_unavailable` text no longer says the requested change "may already have been applied"; it says a change was made only if the record failed after the backend call (#9621).
