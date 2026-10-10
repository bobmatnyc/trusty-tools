Fixed
- Palace alias writes (`register_alias`, `remove_alias`, `rename_target`) now run under the alias file's cross-process lock, so concurrent writers no longer lose updates or share one temp file (#9544).
- `register_alias` and `remove_alias` now refuse a corrupt `palace_aliases.json` and leave its bytes unchanged, instead of overwriting it or answering "no such alias" (#9544).
- The first alias write stamps schema `version: 1`, and a whitespace-only alias file still accepts a write (#9544).
