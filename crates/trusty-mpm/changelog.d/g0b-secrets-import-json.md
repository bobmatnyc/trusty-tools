Added
- `tm secrets import` reads JSON (`--format json`, or any `*.json` file): nested keys flatten to upper-snake names such as `DB_PASSWORD`, and an array or null is refused by key name.
- `tm secrets import --google-oauth <file>` (also detected from an `installed` or `web` object) stores a Google OAuth client's `client_id`, `client_secret` and `project_id` as `google-oauth.<project_id>.CLIENT_ID` and so on. Output names keys only.
