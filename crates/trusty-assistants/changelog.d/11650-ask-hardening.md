Fixed
- An ask replays only the newest logged turns: at most `[ask] max_replay_turns` (default 20) and `[ask] max_replay_chars` characters (default 48000). Older turns stay in `stores/ask-turns.jsonl` but no longer grow every request until it fails on context length (#11650).
