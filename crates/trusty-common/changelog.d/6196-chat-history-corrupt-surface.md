Fixed

- `ChatSessionStore` no longer silently discards a corrupt `history` blob as an
  empty conversation (#6196). All three load paths previously did
  `serde_json::from_str(...).unwrap_or_default()`, so a corrupt session was
  indistinguishable from a genuinely-empty one — `get_session` returned
  `Ok(Some(session))` with `history: []` and a chat-resume caller saw a normal
  new session. Now `get_session` and `append_message`/`append_messages` return
  the new `ChatSessionStoreError::CorruptHistory { id, source }` (append aborts
  its write transaction, so the corrupt row is left intact for recovery rather
  than clobbered), and `list_sessions` skips a corrupt row with a warn and a
  count instead of listing it as a valid empty session.
