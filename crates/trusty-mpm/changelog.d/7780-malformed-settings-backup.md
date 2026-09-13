Fixed

- A project `.claude/settings.json` that is not a JSON object is no longer
  silently overwritten at launch. The three `prepare_session` writers that used
  to coerce it to `{}` — the output-style / auto-memory merge, the plugin
  allowlist, and the project-hooks merge — now copy the original bytes to
  `.claude/settings.json.malformed-<UTC stamp>`, warn naming that copy, and then
  rewrite atomically. A file holding `{ broken` previously came back as tm's own
  keys alone, with nothing kept and nothing logged (#7780).
- A copy that cannot be written refuses the rewrite and leaves the file exactly
  as it was; so does a file that cannot be read at all. The rewrite still
  happens when the copy succeeds, so a project with damaged settings keeps
  getting its PM-guard hook (#1977) rather than launching unguarded (#7780).
- One copy per launch, not one per writer: the first writer repairs the file, so
  every writer after it sees valid JSON (#7780).
