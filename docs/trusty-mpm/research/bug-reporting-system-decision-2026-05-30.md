# ADR: Shared Consent-Gated Bug-Reporting System

**Date**: 2026-05-30  
**Status**: Approved  
**Participants**: Architecture review

## Problem

trusty-* daemons (trusty-search, trusty-memory, trusty-mpm, trusty-analyze) encounter runtime errors that are valuable for developers but potentially expose sensitive information (file paths, user context, system details, secrets) to users. We need a mechanism to:

1. **Capture errors locally** with crate attribution and fingerprinting for deduplication
2. **Gate filing to GitHub** behind explicit user consent
3. **Scrub sensitive data aggressively** before filing
4. **Deduplicate** recurrent errors to avoid spam
5. **Allow developers** to fix bugs without requiring users' personal repo access

## Design: 3-Layer Architecture

### Layer 1: Local Capture (trusty-common)

A tracing Layer (`BugCaptureLayer`) taps ERROR-level events and persists them to a local SQLite database:

- **Store location**: `~/.config/trusty-*/errors.db` (Linux XDG) or `~/Library/Application Support/trusty-*/errors.db` (macOS)
- **Fields captured per error**:
  - Timestamp (UTC)
  - `CARGO_PKG_NAME` (crate name, captured at event source for correct attribution)
  - Crate version
  - Error message and full error chain
  - Code location (file:line)
  - OS and architecture
  - **Fingerprint**: SHA-256(crate + normalized-message + location) for deduplication
- **Behavior**: Opt-out via environment variable; no impact on stderr logging
- **Composition**: Integrates with existing `init_tracing` in `trusty-common/src/lib.rs`

### Layer 2: Surface + Consent (trusty-mpm MCP + HTTP)

MCP tools expose captured errors and gate filing:

- **`list_recent_errors`**: Return recent error records (fingerprint, count, crate, last-seen)
- **`preview_bug_report`**: Show the exact scrubbed payload **without filing**
- **`report_bug`**: File a bug **only after explicit user confirmation**
- **HTTP fallback**: `POST /api/v1/report-bug` on the trusty-mpm daemon (mirrors trusty-memory's `/api/v1/remember` pattern for sub-agents without MCP)

**Acceptance**: Users can list errors, see the exact scrubbed body before filing, and nothing is filed without consent.

### Layer 3: File + Dedup (GitHub App)

A GitHub App (OAuth) manages filing with crate labels and deduplication:

- **App scope**: `issues:write` on `bobmatnyc/trusty-tools`
- **Authentication**: GitHub App installation tokens (short-lived); fallback to a shared bot PAT
- **Issue metadata**:
  - Labels: `bug`, `auto-reported`, `crate/<name>`
  - Hidden fingerprint marker: `<!-- trusty-bug-fingerprint: <hash> -->`
- **Deduplication**: On repeat fingerprint, comment "+1 occurrence" on the existing open issue instead of creating a duplicate
- **Access**: Developers need no personal repo write access; the shared App token handles all filing

**Acceptance**: Confirmed reports create correctly-labeled GitHub issues; repeated fingerprints increment instead of duplicating.

## Decisions

### 1. Consent-Gated (not automatic)
Errors are captured locally, but filing requires explicit user action and preview. This respects user agency and reduces spam.

### 2. Public Repository (trusty-tools) with Aggressive Scrubbing
- All filed issues are public in `bobmatnyc/trusty-tools`
- Conservative default scrubbing:
  - File paths → `~`
  - Usernames → redacted
  - Tokens, keys, JWTs → stripped
  - Environment values → scrubbed
  - Payload truncated to 16 KB
- Users can preview the scrubbed body before filing

### 3. Shared GitHub App Token (not per-user PAT)
A single App reduces operational burden and prevents token sprawl. No user needs personal repo write access.

### 4. Crate-Tagged Issues
Every issue includes the originating crate as a label (`crate/<name>`). This allows GitHub filtering and project triage.

### 5. Fingerprint Deduplication
SHA-256(crate + normalized-message + location) deduplicates recurrent errors. Hidden `<!-- -->` comments prevent accidental collision with user-generated issues.

## Implementation Phases

See linked tickets for detailed acceptance criteria:

1. **Phase 1 (trusty-common)**: Error capture layer with SQLite + fingerprint
2. **Phase 2 (trusty-mpm MCP + HTTP)**: Surface + consent tools
3. **Phase 3 (GitHub App)**: Filing + dedup
4. **Phase 4 (hardening)**: Scrubbing polish, rate limits, docs

## Trade-offs

| Aspect | Choice | Alternative | Rationale |
|--------|--------|-------------|-----------|
| Storage | Local SQLite | In-memory ring buffer | SQLite survives crashes; enables long-tail analysis |
| Consent | Explicit user action | Automatic + opt-out | Respects user agency; fewer UX surprises |
| Repository | Public (trusty-tools) | Private; per-user fork | Public centralizes bugs; easier to triage and fix |
| Scrubbing | Conservative defaults | Minimal | Reduces accidental secret leaks; users can review |
| Token model | Shared GitHub App | Per-user PAT | Scales without token sprawl; no personal access required |
| Dedup | Fingerprint + comment | Duplicate detection + comment | Fingerprint is deterministic and portable across language boundaries |

## Success Metrics

1. **Adoption**: >50% of daemon runs have error capture enabled (Phase 1 rollout)
2. **Quality**: Bug reports reduce manual triage time by >30% (crate labels + fingerprint dedup)
3. **Privacy**: Zero secret leaks in filed issues (automated scrubbing + user review)
4. **Spam**: <5% false-positive duplicates after Phase 4 rate limits

## Open Questions

- Should we expose error counts in telemetry (e.g., to product dashboards)?
- How frequently should we auto-upload captured errors if the user has never filed a report (if at all)?
- Should we offer a "report anonymously" mode that strips CARGO_PKG_NAME for unsupported crates?

## Related Links

- [Epic issue](link_will_be_updated_after_filing)
- Phase 1, 2, 3, 4 tickets (linked in epic)
