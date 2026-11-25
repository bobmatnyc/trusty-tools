Added
- `ask::ask(home, question)` and `ask::AskEngine` answer one question as an assistant instance on trusty-code's `AgentLoop`. The system prompt is the instance's `instructions.md` plus a note naming its primary OKG; an instructions error is a typed `AskError` and no model call is made (#11650).
- The ask registry holds only allowlisted read-only MCP tools plus `okg_search`, a text search rooted at the instance's `okg/` directory. Write and send tools, and every coding tool, are left out (#11650).
- Each instance keeps an append-only turn log at `stores/ask-turns.jsonl`. The next ask replays it, a corrupt line is a typed error, and a failed or cancelled ask never writes to it (#11650).
- `config.toml` takes an `[ask]` table with `model`, `max_turns` and `timeout_secs`. An absent table uses 8 turns and 120 s, and an absent or blank `model` uses the crate default model (#11650).
