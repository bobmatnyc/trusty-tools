Changed
- `secrets.copy` to a sync target (GitHub Actions, Vercel) now admits a caller with a Claude Code ancestor for keys not denied to agents, superseding D63 D3 for agents (#10999, owner ruling 2026-11-10). A key denied to agents fails with `agent_key_denied` before its value is read; an unreadable ancestry chain still refuses with `grant_refused`. Each key's audit record names the target, key, caller pid and `agent_parent`, never the value.
- The Vercel "already existed" (D9) failure reason now says the new value may already be set and a retry reports the same failure (#10999).
- An agent may push a key marked push-only for agents (#10998) to a sync target; a deny still refuses and wins over the mark, and a failed lookup of the deny or the mark fails the key (#10999).
