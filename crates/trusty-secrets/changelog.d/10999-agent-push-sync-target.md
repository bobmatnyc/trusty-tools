Changed
- `secrets.copy` to a sync target (GitHub Actions, Vercel) now admits a caller with a Claude Code ancestor for keys not denied to agents, superseding D63 D3 for agents (#10999, owner ruling 2026-11-10). A key denied to agents fails with `agent_key_denied` before its value is read; an unreadable ancestry chain still refuses with `grant_refused`. Each key's audit record names the target, key, caller pid and `agent_parent`, never the value.
- The Vercel "already existed" (D9) failure reason now says the new value may already be set and a retry reports the same failure (#10999).
