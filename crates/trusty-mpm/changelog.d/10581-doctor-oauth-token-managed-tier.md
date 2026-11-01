Fixed
- `tm doctor` oauth_token no longer warns when Claude Code already holds a login under the managed `CLAUDE_CONFIG_DIR` (a `.credentials.json` there, or the `oauthAccount` login record in its `.claude.json`), as on a supervisor install. With no login evidence in that tier, or when that file is unreadable or not JSON, it still warns (#10581).
