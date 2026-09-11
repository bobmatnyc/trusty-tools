Fixed
- `pm_guard`'s secret-file class no longer refuses a one-character glob fragment such as `s*`: the word-family shape gate now uses the same glob matcher the deny used, so a candidate reaching only `credentials`/`secrets`/`token` must still be written as a path (Refs #7498).
- `pm_guard` reads `.env.example`, `.env.sample` and `.env.template` as placeholders rather than secrets, keyed on the final extension so `.env.example.bak` still denies (Refs #7479).
- `pm_guard` treats a comma-free brace group as literal text the way a shell does, so a Go-template argument such as `docker ps --format '{{.ID}}'` allows while a real brace alternation still resolves or fails closed (Refs #7499).
- `pm_guard`'s own verdict for the read-only commands reported as nondeterministically refused is pinned as classifiable, allowed and stable across repeated evaluation; the refusal text belongs to the Claude Code harness, not to `tm` (Refs #7477, Refs #7436).
