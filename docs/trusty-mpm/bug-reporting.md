# Bug-Reporting System — User and Developer Guide

**Version**: Phase 4 (scrubber hardening, GitHub App auth, rate-limiting)  
**ADR**: [`research/bug-reporting-system-decision-2026-05-30.md`](research/bug-reporting-system-decision-2026-05-30.md)

---

## What it does

The trusty-mpm bug-reporting system captures runtime errors from all trusty-*
daemons (trusty-search, trusty-memory, trusty-mpm, trusty-analyze) and lets you
file them as GitHub issues in `bobmatnyc/trusty-tools` — with your explicit
consent, after reviewing a scrubbed preview.

Key properties:
- **Nothing files automatically.** You must call `report_bug` and confirm.
- **You see exactly what gets filed** before it happens (preview = filed body).
- **Sensitive data is scrubbed** before you review the preview — secrets, paths,
  and usernames are replaced with redaction placeholders.
- **Deduplication** — if the same error (by fingerprint) already has an open
  issue, a "+1 occurrence" comment is posted instead of creating a duplicate.
- **Rate-limiting** — the same fingerprint cannot be re-filed within 24 hours;
  at most 10 new issues per rolling hour per machine.

---

## Consent model

```
capture (local) → list → preview → YOU CONFIRM → file
```

1. Errors are captured locally to `~/.config/trusty-*/errors.db` (never sent
   anywhere without your action).
2. You use `list_recent_errors` to see what has been captured.
3. You use `preview_bug_report` to see the exact scrubbed Markdown that would
   be filed.
4. You call `report_bug` with `confirm: true` to actually file the issue.

At no point does the system file anything without an explicit confirm.

---

## Opt-out

Set `TRUSTY_NO_BUG_CAPTURE=1` in your environment to disable local error
capture entirely. No errors will be captured or stored; the MCP tools will
return empty results.

---

## Authentication

Authentication is required only when filing (`report_bug` with `confirm: true`).
Previewing errors (`preview_bug_report`) works without any token.

### Resolution order

The system tries token sources in this order:

1. **PAT env var** `TRUSTY_BUGREPORT_GITHUB_TOKEN` — a Personal Access Token
   with `issues:write` on `bobmatnyc/trusty-tools`. Fastest to set up; suitable
   for solo developers.
2. **Token file** — `TRUSTY_BUGREPORT_TOKEN_FILE` (override path) or
   `~/.config/trusty-mpm/bugreport-token` (default). Same PAT stored in a file
   (useful for CI, dotfiles).
3. **GitHub App** — if all three App env vars are set
   (`TRUSTY_BUGREPORT_GH_APP_ID`, `TRUSTY_BUGREPORT_GH_INSTALL_ID`,
   `TRUSTY_BUGREPORT_GH_APP_KEY_FILE`), the App mints a short-lived installation
   token automatically. Recommended for teams.
4. **None** — `report_bug` returns a `NoToken` error with instructions. Previewing
   still works.

### Quick-start: PAT (solo developer)

```bash
# Create a GitHub PAT with Issues: Write on bobmatnyc/trusty-tools, then:
export TRUSTY_BUGREPORT_GITHUB_TOKEN=ghp_your_token_here

# Or write it to the default file:
mkdir -p ~/.config/trusty-mpm
echo "ghp_your_token_here" > ~/.config/trusty-mpm/bugreport-token
chmod 600 ~/.config/trusty-mpm/bugreport-token
```

### GitHub App setup (team-recommended)

The GitHub App approach avoids token sprawl: each developer needs no personal
repo access. One App handles all filing.

1. **Create the App**: GitHub → Settings → Developer settings → GitHub Apps →
   New GitHub App.
   - Name: `trusty-bug-reporter` (or similar)
   - Permissions: `Issues: Write` (Repository permission)
   - Webhook: unchecked
2. **Generate a private key**: On the App page → "Generate a private key".
   Save the downloaded `.pem` file somewhere accessible (e.g.
   `~/.config/trusty-mpm/app-key.pem`).
3. **Install the App** on `bobmatnyc/trusty-tools`: On the App page → "Install
   App" → select the repo. Note the numeric installation ID from the install URL
   (e.g. `https://github.com/settings/installations/12345678` → ID is `12345678`).
4. **Note the App ID** from the App's "About" section.
5. **Set the three env vars**:

```bash
export TRUSTY_BUGREPORT_GH_APP_ID=<numeric App ID>
export TRUSTY_BUGREPORT_GH_INSTALL_ID=<numeric installation ID>
export TRUSTY_BUGREPORT_GH_APP_KEY_FILE=/path/to/app-key.pem
```

The provider mints a 60-minute installation token, caches it, and refreshes
automatically 5 minutes before expiry. No long-lived secrets are needed in
environment variables.

---

## Privacy and scrubbing

Before you see the preview (and certainly before anything is filed), the body
is passed through the scrubber. The following patterns are redacted:

| Category | Examples | Replacement |
|---|---|---|
| PEM private-key blocks | `-----BEGIN RSA PRIVATE KEY-----` | `[REDACTED_PRIVATE_KEY]` |
| Bearer / Authorization headers | `Authorization: Bearer ...` | `[REDACTED_TOKEN]` |
| JWT strings | `eyJ...` | `[REDACTED_JWT]` |
| LLM API keys | `sk-...`, `sk-ant-...`, `sk-or-...` | `[REDACTED_API_KEY]` |
| GitHub tokens | `ghp_...`, `gho_...`, `ghu_...`, `ghs_...` | `[REDACTED_GITHUB_TOKEN]` |
| AWS access keys | `AKIA...` | `[REDACTED_AWS_KEY]` |
| Google API keys | `AIza...` | `[REDACTED_GOOGLE_KEY]` |
| Slack tokens | `xoxb-...`, `xoxp-...` | `[REDACTED_SLACK_TOKEN]` |
| Connection strings | `postgres://user:pass@host` | `[REDACTED_CONN_STRING]` |
| Env-KV secrets | `TOKEN=...`, `SECRET=...`, `API_KEY=...` | `KEY=[REDACTED_VALUE]` |
| POSIX absolute paths | `/Users/alice/...`, `/home/bob/...` | `~` |
| Windows paths | `C:\Users\alice\...` | `~` |
| Truncation | Body > 16 KiB | Truncated with notice |

The preview shows a **redaction summary** (e.g. `"3 secrets, 2 paths redacted"`)
and the full list of what was changed. If you see something that should not be
filed, you can decline — nothing is sent until you confirm.

The scrubber is **intentionally conservative**: false positives (non-secret text
that looks like a secret) are safer than false negatives (a real secret in a
public issue).

---

## Deduplication

Every error has a SHA-256 fingerprint computed from `crate + normalized message + location`.

When you confirm filing:
1. The system searches GitHub for an open issue with a hidden marker
   `<!-- trusty-bug-fingerprint: <fp> -->`.
2. **If found**: posts a "+1 occurrence" comment with the date and fingerprint.
   No new issue is created.
3. **If not found**: creates a new labeled issue.

This prevents duplicate issues for the same recurring error across multiple
developers or runs.

---

## Rate-limiting

Two guards prevent accidental spam:

**Per-fingerprint stamp** (24-hour window)  
After a fingerprint is filed, the same fingerprint cannot be filed again for
24 hours from the same machine. This complements the GitHub-side dedup
(which handles cross-machine). State is stored in
`~/.config/trusty-mpm/bugreport-fp-stamps.json`.

**Per-hour cap** (10 issues/hour)  
At most 10 new issues can be filed per rolling hour per machine. If the cap is
hit, `report_bug` returns a `rate-limited` error with the count and a note to
try later. State is stored in `~/.config/trusty-mpm/bugreport-hourly.json`.

Both limits reset automatically (the 24h stamp expires; hourly timestamps roll
off the window). The state files are human-readable JSON if you need to inspect
or reset them manually.

---

## MCP tools

These tools are exposed to Claude Code and any MCP-capable client:

| Tool | Purpose |
|---|---|
| `list_recent_errors` | List captured errors (fingerprint, count, crate, last seen) |
| `preview_bug_report` | Show the scrubbed preview without filing |
| `report_bug` | File an issue; requires `confirm: true` to actually file |

### Example workflow (Claude Code)

```
> list_recent_errors

Found 2 captured errors:
  1. trusty_search::indexer — "index open failed" (3 occurrences)
     Fingerprint: a1b2c3...
  2. trusty_memory — "palace lock timeout" (1 occurrence)
     Fingerprint: d4e5f6...

> preview_bug_report fingerprint=a1b2c3...

Redaction summary: 1 path redacted
---
## Auto-reported error
...

> report_bug fingerprint=a1b2c3... confirm=true

Filed: https://github.com/bobmatnyc/trusty-tools/issues/501
```

---

## HTTP endpoint (sub-agent fallback)

Sub-agents that cannot call MCP tools directly can POST to the daemon's HTTP
endpoint:

```http
POST /api/v1/report-bug
Content-Type: application/json

{
  "fingerprint": "a1b2c3...",
  "confirm": true
}
```

Response on success:
```json
{
  "filed": true,
  "result": {
    "filed": true,
    "deduped": false,
    "issue_url": "https://github.com/bobmatnyc/trusty-tools/issues/501",
    "issue_number": 501
  }
}
```

Omitting `confirm` or setting it to `false` returns a preview-only response
(`"filed": false`, `"note": "preview only — set confirm:true to file"`).

---

## Labeled issues

Every filed issue gets:
- `bug` — always applied
- `auto-reported` — always applied
- `trusty-search` / `trusty-memory` / `trusty-mpm` / etc. — crate-specific label
  when the crate is recognised

Unknown crates get only the two base labels.

---

## Implementation notes for contributors

- Scrubber: `crates/trusty-mpm/src/daemon/bug_report/scrubber.rs`
- Token providers: `crates/trusty-mpm/src/daemon/bug_report/token.rs`
- Rate-limit guards: `crates/trusty-mpm/src/daemon/bug_report/ratelimit.rs`
- GitHub client + dedup: `crates/trusty-mpm/src/daemon/bug_report/github.rs`
- Preview builder: `crates/trusty-mpm/src/daemon/bug_report/preview.rs`
- Error aggregation: `crates/trusty-mpm/src/daemon/bug_report/multi_store.rs`

Run the full test suite with:
```bash
cargo test -p trusty-mpm
```
