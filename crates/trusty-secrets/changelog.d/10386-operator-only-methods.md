Added
- `secrets.set` and `secrets.delete` refuse a caller with a Claude Code ancestor (`agent_use_refused`), directly or through the console bridge; an unreadable ancestry or a missing peer pid refuses (`grant_refused`). A copy to a sync target and removing a deny stay refused to agents; writing a deny is allowed (#10386).
- The file backend persists the per-key agent deny as a 0600 file under `<values root>/.agent-controls/`, so a host with no Keychain can deny a key to agents (#10386, subsumes #10090).
- Grant and resolve allow records for a caller under Claude Code carry `default_policy: true` (#10386).
- `server::serve_without_agent_callers` (`test-support` feature only): a server whose process table reports no Claude Code process, so a consumer's suite that stores keys through a real server passes the same in CI and in an agent session (#10386).
