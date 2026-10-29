Fixed
- The Architect Intent Ledger hook (`datg-hook.sh`) keeps its `SessionStart` context, including after `/clear` and compaction, under Claude Code's 10,000-character hook limit (#10109). A digest cut to fit ends with the `datg.py digest` command that prints it in full.
- The Intent Ledger hook prints nothing for a session without a ledger, and reports an unreadable ledger with one `datg hook: failed open` line on stderr instead of failing silently (#10109). The session still starts.
