Fixed

- Every MCP boolean flag now rejects a wrong-typed value with an `INVALID_PARAMS` error naming the parameter and the expected type, instead of silently reading it as the default. `exclude_archived`, `serial`, `full_content`, `include_source`, `follow_links`, `check`, `confirm`, and `grep`'s `case_insensitive` / `multiline` / `fixed_strings` / `files_with_matches` / `invert_match` / `word_regexp` used to map `"true"` (a string) onto `false` with nothing in the response to say the flag was dropped. An absent flag still means the documented default (#7927).
