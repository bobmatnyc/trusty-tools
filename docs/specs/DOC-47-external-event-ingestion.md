# DOC-47: External Event Ingestion

**Status:** Draft  
**Spec ID:** `SPEC-EVTING-01~draft` (and -02~draft below)  
**Scope:** Workspace-wide (trusty-agents-common, trusty-mpm, trusty-console, adapters)  
**Tracking Issue:** #3090  
**Related:** ADR-0014 (shared ingress via console + Tailscale Funnel), ADR-0005 (event bus)

---

## Executive Summary

This spec defines **push-based ingestion of external events** — GitHub issues, JIRA tickets, connector notifications (Gmail, Slack, Telegram) — into the trusty harness event system without polling. Registered projects' incoming events (GitHub App installations, JIRA automation webhooks, connector socket-mode subscriptions) automatically flow into the SM's goal store, seeding sessions for orchestration. The design prioritizes zero cloud infrastructure (Tailscale Funnel), per-source cryptographic verification, and unified transport across disparate source types (webhooks, held connections, and a poll fallback).

---

## Overview — Why Push-Based Ingestion

### The Problem

Today, discovering work that needs agent attention requires periodic polling:
- GitHub issue updates (pull-every-N-minutes)
- JIRA automation triggers (pull-every-N-minutes, or periodic sync)
- Slack/Gmail/Telegram messages (connector long-poll or periodic-check)

Polling is:
- **Latency-bound** — events arrive 1–5 minutes after creation, delaying response
- **Resource-heavy** — redundant API calls, rate-limit friction, storage churn
- **Ownership-unclear** — no canonical source of truth per project; polling scatter across PM + connectors

### The Solution

Push-based ingestion (webhooks, socket-mode subscriptions, IMAP IDLE) delivers events **when created**, not when polled. A **shared ingress** routes all webhook-class sources through a single HTTP endpoint, eliminating per-source endpoint sprawl. The design normalizes delivery classes (true push, held-connection, poll fallback) behind a transparent `EventSource` contract so consumers see one unified interface.

---

## Ingestion Architecture

### Seam 1: EventSource Contract {#SPEC-EVTING-01~draft}

A new `EventSource` trait in `trusty-agents-common::events` models external event producers. Three delivery classes implement it; consumers interact only with the contract:

```rust
/// Why: normalize disparate external event sources (webhook, socket-mode, IMAP IDLE, poll) 
/// into one interface so consumer code is independent of delivery mechanism.
/// 
/// What: EventSource trait accepts push/pull events and normalizes them to ExternalEvent payloads,
/// which are published to the HarnessEvent bus via the External payload arm.
/// 
/// Test: crates/trusty-agents-common/src/events/sources/tests.rs covers adapter instantiation,
/// event normalization, and bus publication per class.
pub trait EventSource: Send + Sync {
    fn source_name(&self) -> &str;  // "github", "jira", "slack", "telegram", "gmail", "calendar"
    fn source_kind(&self) -> SourceKind; // Push, HeldConnection, Poll
    
    async fn next(&mut self) -> Result<Option<ExternalEvent>>;
}

/// Why: classify delivery model for transparent fallback and diagnostics.
pub enum SourceKind {
    Push,            // Webhook, incoming HTTP
    HeldConnection,  // Socket Mode (Slack/Telegram), IMAP IDLE (Gmail), push channels (Calendar)
    Poll,            // Fallback; queried on demand only
}

/// Why: unify external event shape across all sources before normalization to HarnessEvent payload.
/// 
/// What: carries the raw event data, delivery ID (for idempotency), and project registry link.
/// Delivered-at timestamp differentiates between ingestion time (agent's now()) and source time.
/// 
/// Test: serialization round-trips per source type; idempotency key uniqueness within project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalEvent {
    pub source: String,              // "github", "jira", etc.
    pub delivery_id: String,         // unique per source (GitHub X-Delivery-ID, JIRA issue-key, etc.)
    pub kind: String,                // "issue.opened", "issue.assigned", "message", etc.
    pub data: serde_json::Value,     // raw event payload
    pub project_hint: Option<String>, // e.g., repo owner/name (for early project matching)
    pub delivered_at: DateTime<Utc>, // system time when ingested
}
```

**Normalization to HarnessEvent:** The External arm of `HarnessPayload` (ADR-0005) wraps the `ExternalEvent`:

```rust
pub enum HarnessPayload {
    Lifecycle(LifecycleEvent),
    Hook { kind: String, data: serde_json::Value },
    External {
        source: String,      // "github", "jira", "slack", "telegram", "gmail", "calendar"
        kind: String,        // "issue.opened", "issue.assigned", "message", etc.
        data: serde_json::Value, // the full ExternalEvent serialized
    },
    Ping,
}
```

---

### Seam 2: Shared Webhook Ingress {#SPEC-EVTING-02~draft}

#### Route & Binding

A **single public HTTPS endpoint** serves all webhook-class sources:

```
POST /api/webhooks/{source}
```

Examples:
- `POST /api/webhooks/github` — GitHub App webhook
- `POST /api/webhooks/jira` — JIRA Connect/automation webhook
- `POST /api/webhooks/slack` — Slack Events API (optional; Socket Mode is primary)
- `POST /api/webhooks/gmail` — Google Cloud Pub/Sub push (optional; IMAP IDLE is primary)

**Transport:** Routed through `trusty-console`'s existing reverse proxy (crates/trusty-console/src/server.rs, `ANY /api/{service}/{*path}`), which forwards to a new webhook receiver mounted in `trusty-mpm`'s daemon API (`crates/trusty-mpm/src/daemon/api.rs`, beside the existing `POST /hooks` handler, see issue #1970 for precedent).

**Public Access:** Via `trusty-console`'s `BindMode` (crates/trusty-console/src/bind.rs), extended with a **Tailscale Funnel mode** (issue #3089):
- `--tailscale` existing mode: tailnet-only access
- `--funnel` new mode (default when internet-facing): public relay via Tailscale's regional funnel servers, zero local inbound firewall rules, 1:1 forwarding, upstream-signed TLS
- Fallback: A documented later upgrade path (phase 2) is a thin cloud relay (receive + queue + outbound-forward), not in scope of this spec

#### Signature Verification

Before any processing, the ingress verifies the source's cryptographic signature **synchronously at the receiver** — all other logic is gated behind it.

**Per-source verification methods** (all preceding the ExternalEvent normalization):

- **GitHub App webhooks:** X-Hub-Signature-256 HMAC-SHA256 using the app's webhook secret (reuse pattern from trusty-review #553, GitHub App identity from #550)
- **Slack:** X-Slack-Request-Timestamp + X-Slack-Signature (HMAC-SHA256 of timestamp + request body)
- **Telegram:** Validate bot token in URL path or bearer token + verify message hash
- **JIRA Connect:** X-Atlassian-Signature (HMAC-SHA256) or JWT (depending on Connect vs automation)
- **Google Cloud Pub/Sub:** Bearer token or Google-signed ID token (project-scoped)
- **Calendar (Google/Outlook):** per-integration bearer token or channel verification token

**Credential Storage:** Follow DOC-45 model (issue #3038) — OAuth tokens and signing secrets live in:
- Environment variables (TRUSTY_GITHUB_WEBHOOK_SECRET, TRUSTY_SLACK_SIGNING_SECRET, etc.)
- `.trusty-tools/integrations/{source}.json` (local, encrypted at rest if at-rest encryption is configured)
- Never URL-embedded, never logged in plaintext

Ingress handler pseudocode:

```rust
async fn ingest_webhook(
    source: String,
    headers: HeaderMap,
    body: Bytes,
    store: &SmGoalStore,
    project_resolver: &ProjectResolver,
) -> Result<Response, IngestError> {
    // 1. Verify signature (sync, first)
    verify_signature(&source, &headers, &body)?;
    
    // 2. Normalize to ExternalEvent
    let event = normalize_to_external(&source, &headers, &body)?;
    
    // 3. Resolve project via registry
    let project = project_resolver.resolve_from_event(&event)?;
    
    // 4. Create durable Goal with dedup
    let goal = Goal::from_external_event(&event, &project)?;
    let goal_id = store.upsert_dedup(&goal).await?;
    
    // 5. Publish to HarnessEvent bus (best-effort)
    let seq = HarnessEvent::publish_external(
        &source, &event.kind, &event.data
    );
    
    // 6. Return 202 Accepted (or 200 OK if idempotent duplicate)
    Ok(Response::builder()
        .status(202)
        .header("X-Event-ID", &event.delivery_id)
        .header("X-Goal-ID", &goal_id)
        .json(json!{"seq": seq})?)
}
```

---

## Event Model — External Events → Goals

### Project Resolution & Registry

Before ingesting, the receiver must **resolve the external event to a registered project**. The `ProjectResolver` (crates/trusty-mpm/src/project/resolver) matches the event's project hint against the project registry:

```rust
impl ProjectResolver {
    /// Why: bind an external event to a project without manual routing setup.
    /// 
    /// What: match event's repository (GitHub), board (JIRA), workspace (Slack), etc.,
    /// against registered project URLs/slugs to find the responsible project.
    /// 
    /// Test: crates/trusty-mpm/src/project/resolver/tests.rs covers GitHub owner/repo matching,
    /// JIRA project key matching, Slack workspace/channel matching, and fallback to global inbox.
    pub async fn resolve_from_event(&self, event: &ExternalEvent) -> Result<Project> {
        // Match against project registry entries (issue #3091: registry schema)
        // Priority: exact repo/project match > workspace-scoped > global inbox
        todo!()
    }
}
```

### Goal Lifecycle

Each external event becomes a **durable Goal** in the SM's goal store (`crates/trusty-mpm/src/core/sm/goals/`, DOC-14 §9):

```rust
pub struct Goal {
    pub id: GoalId,
    pub project: Project,
    pub source: ExternalEventSource,  // "github", "jira", "slack", etc.
    pub source_event_id: String,      // GitHub #123, JIRA PROJ-456, Slack ts=12345.67890
    pub kind: String,                 // "issue.opened", "issue.assigned", etc.
    pub title: String,                // human-readable summary
    pub description: String,          // full event details
    pub state: GoalState,             // Created, Active, Paused, Done
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub links: HashMap<String, String>, // link to source (GitHub issue URL, etc.)
}

pub enum GoalState {
    Created,    // Just ingested, awaiting session spawn
    Active,     // Session spawned from goal, in flight
    Paused,     // Session paused (user action)
    Done,       // Completed or archived
}

impl Goal {
    /// Why: normalize an external event into a durable goal so multiple agents
    /// can be spawned to act on it without re-fetching the source.
    /// 
    /// What: extract title, description, priority from event payload (source-specific);
    /// generate a deterministic goal ID from source + delivery ID for idempotency.
    /// 
    /// Test: crates/trusty-mpm/src/core/sm/goals/tests.rs covers per-source normalization,
    /// ID stability across re-ingestion, and dedup semantics.
    pub fn from_external_event(event: &ExternalEvent, project: &Project) -> Result<Self> {
        // Source-specific extraction logic (see adapters below)
        todo!()
    }
}

impl SmGoalStore {
    /// Why: ensure one goal per source delivery (idempotency).
    /// 
    /// What: given a goal, check if one already exists with the same (source, source_event_id).
    /// If yes, update state and return existing ID; if no, create new.
    /// 
    /// Test: crates/trusty-mpm/src/core/sm/goals/tests.rs covers create, re-ingest-same-delivery, 
    /// and update-on-re-receipt scenarios.
    pub async fn upsert_dedup(&self, goal: &Goal) -> Result<GoalId> {
        todo!()
    }
}
```

### SessionLink — From Goal to Session

When a goal becomes active (either automatically or via PM decision), the SM spawns a session linked to that goal:

```rust
pub struct SessionLink {
    pub goal_id: GoalId,
    pub session_id: String,
}

// No new notification path; sessions already spawn from existing triggers
// (tm new, tm project, delegated agent). A goal is simply another trigger source.
```

Sessions do **not** re-fetch the source; they consume the goal's stored event data, links, and context. If the agent needs live state (e.g., current GitHub issue status), it queries the source directly via a connector MCP or trusty-search query.

---

## Per-Source Ingestion Matrix {#SPEC-EVTING-03~draft}

| Source | Delivery Class | Verification Method | Webhook Endpoint | Current Status | Scope |
|--------|---|---|---|---|---|
| **GitHub** | Push (webhook) | X-Hub-Signature-256 | `/api/webhooks/github` | Ready (App infra from #550) | Org-scoped installations; capture issue.opened, issue.assigned, pr.opened, comment.created |
| **JIRA** | Push (automation) | X-Atlassian-Signature | `/api/webhooks/jira` | Optional (no admin needed for automation rules) | Project-scoped; capture issue.created, issue.assigned, issue.transitioned |
| **Slack** | HeldConnection (Socket Mode) | Token + event type | — (no webhook; keep Socket Mode) | Implemented (crates/trusty-agents/src/slack/); not yet publishing to bus | app_mention, message, reaction_added per subscribed channels |
| **Slack** | Push (Events API) | X-Slack-Signature | `/api/webhooks/slack` (optional) | Future upgrade | Same event types; webhook is opt-in redundancy to Socket Mode |
| **Telegram** | HeldConnection (long-poll) | Bot token in URL | — (no webhook; keep long-poll) | Implemented (crates/trusty-agents/src/telegram/); not yet publishing to bus | message, callback_query per subscribed chats |
| **Telegram** | Push (setWebhook) | Secret token | `/api/webhooks/telegram` (optional) | Future upgrade | Same events; webhook is opt-in redundancy to long-poll |
| **Gmail** | HeldConnection (IMAP IDLE) | OAuth scope `mail.readonly` | — (IMAP, no webhook) | Phase 1 (fall-2026 roadmap) | Inbox IDLE notifications; message subjects, senders |
| **Gmail** | Push (Cloud Pub/Sub) | Google-signed ID token | `/api/webhooks/gmail` (optional) | Phase 2 (spring-2027 roadmap) | GCP project + topic subscription; 7-day expiry with automatic renewal |
| **Calendar** (Google/Outlook) | Push (channels API) | Bearer token (per integration) | `/api/webhooks/calendar` | Phase 1 (fall-2026 roadmap) | Event.created, event.updated per subscribed calendars; 7-day expiry |

**Notes:**
- **HeldConnection sources (Slack, Telegram, Gmail IMAP)** maintain open subscriptions and push events through those connections; they **do not** use the `/api/webhooks/` endpoint. Instead, their adapters are `EventSource` implementations that emit via the HarnessEvent bus.
- **Webhook-class sources (GitHub, JIRA, optional Slack/Telegram/Gmail/Calendar)** POST to `/api/webhooks/{source}` and require signature verification.
- **Poll fallback** is a quarantined last resort; it is implemented inside adapters (e.g., if Socket Mode disconnects, Slack adapter falls back to Events API or a periodic poll) and is never the primary mechanism for any source in this phase.
- **JIRA:** Support both automation rules (no admin needed; user creates the rule per project) and Connect webhooks (admin-install scoped to entire instance). Automation rules are the zero-install path for projects without Connect.

---

## Security Model {#SPEC-EVTING-04~draft}

### Signature Verification (Mandatory)

All webhook ingress handlers **must** verify the signature before deserializing or processing the body. Verification failures return **401 Unauthorized**; any other ingestion error returns **400 Bad Request**.

Pseudocode (generic):

```rust
fn verify_signature(
    source: &str,
    headers: &HeaderMap,
    body: &Bytes,
    secrets: &Secrets, // per-source secret store
) -> Result<()> {
    match source {
        "github" => {
            let sig = headers
                .get("x-hub-signature-256")?
                .to_str()?
                .strip_prefix("sha256=")?;
            let secret = secrets.get("github_webhook_secret")?;
            let expected = hmac_sha256(body, secret.as_bytes());
            if !constant_time_eq(sig.as_bytes(), &expected) {
                return Err(IngestError::Unauthorized("invalid signature"));
            }
        }
        "slack" => {
            let timestamp = headers
                .get("x-slack-request-timestamp")?
                .to_str()?;
            let sig = headers
                .get("x-slack-signature")?
                .to_str()?
                .strip_prefix("v0=")?;
            // Timestamp must be within 5 min of now
            if (now() - timestamp.parse::<i64>()?) > 300 {
                return Err(IngestError::Unauthorized("timestamp too old"));
            }
            let secret = secrets.get("slack_signing_secret")?;
            let signed = format!("v0:{}:{}", timestamp, String::from_utf8(body.to_vec())?);
            let expected = hmac_sha256(signed.as_bytes(), secret.as_bytes());
            if !constant_time_eq(sig.as_bytes(), &expected) {
                return Err(IngestError::Unauthorized("invalid signature"));
            }
        }
        // ... similar for JIRA, Telegram, Google, etc.
        _ => Err(IngestError::UnknownSource),
    }
    Ok(())
}
```

### Credential Storage & Rotation

Secrets are stored and rotated following DOC-45 model (issue #3038; OAuth/env-injection preferred):

- **Environment variables** (deployment-time injection): `TRUSTY_{SOURCE_UPPER}_WEBHOOK_SECRET`
- **Local credential file** (for dev): `.trusty-tools/integrations/{source}.json` (never checked in; .gitignored)
- **Rotating OAuth tokens** (GitHub App, Google): token refresh is handled by trusty-common's auth module (issue #550 precedent)
- **Never log secrets** in info/debug logs; sanitize them in error messages

---

## Event Adaptation (Source-Specific Normalization)

Each `EventSource` adapter implements source-specific extraction logic. Three examples:

### GitHub Example

```rust
// crates/trusty-mpm/src/adapters/github_webhook.rs

struct GitHubWebhookSource;

impl EventSource for GitHubWebhookSource {
    fn source_name(&self) -> &str { "github" }
    fn source_kind(&self) -> SourceKind { SourceKind::Push }
    
    async fn next(&mut self) -> Result<Option<ExternalEvent>> {
        // This is for the held-connection/poll case; webhooks are handled by the ingress.
        // For GitHub, this would be a fallback poll of the app's received events queue.
        todo!()
    }
}

// Webhook handler (mounted at POST /api/webhooks/github)
pub async fn handle_github_webhook(
    headers: HeaderMap,
    body: Bytes,
    store: &SmGoalStore,
) -> Result<impl IntoResponse> {
    let event_type = headers.get("x-github-event")?.to_str()?;
    
    let external_event = match event_type {
        "issues" => {
            let gh_payload: serde_json::Value = serde_json::from_slice(&body)?;
            ExternalEvent {
                source: "github".to_string(),
                delivery_id: headers.get("x-github-delivery")?.to_string(),
                kind: format!("issues.{}", gh_payload["action"]),
                data: gh_payload,
                project_hint: Some(format!(
                    "{}/{}",
                    gh_payload["repository"]["owner"]["login"],
                    gh_payload["repository"]["name"]
                )),
                delivered_at: Utc::now(),
            }
        }
        "pull_request" => { /* similar */ todo!() }
        "pull_request_review_comment" => { /* similar */ todo!() }
        _ => return Err(IngestError::UnknownAction),
    };
    
    // ... rest of ingest_webhook flow (resolve, upsert, publish)
    todo!()
}
```

### JIRA Example

```rust
// crates/trusty-mpm/src/adapters/jira_webhook.rs

pub async fn handle_jira_webhook(
    headers: HeaderMap,
    body: Bytes,
    store: &SmGoalStore,
) -> Result<impl IntoResponse> {
    // JIRA webhooks include an issue key and event type in the payload
    let payload: serde_json::Value = serde_json::from_slice(&body)?;
    
    let external_event = ExternalEvent {
        source: "jira".to_string(),
        delivery_id: payload["webhookEvent"]
            .as_str()
            .map(|s| format!("{}-{}", payload["issue"]["key"], s))
            .ok_or(IngestError::MissingField)?,
        kind: payload["webhookEvent"].as_str().unwrap_or("unknown").to_string(),
        data: payload.clone(),
        project_hint: payload["issue"]["fields"]["project"]["key"]
            .as_str()
            .map(|s| s.to_string()),
        delivered_at: Utc::now(),
    };
    
    // ... rest of ingest_webhook flow
    todo!()
}
```

### Slack Socket Mode Example (HeldConnection)

```rust
// crates/trusty-agents/src/slack/adapter.rs (NEW: publish to bus)

struct SlackSocketModeSource {
    client: slack_sdk::events::EventsClient,
    subscriptions: Vec<String>, // channel IDs
}

#[async_trait]
impl EventSource for SlackSocketModeSource {
    fn source_name(&self) -> &str { "slack" }
    fn source_kind(&self) -> SourceKind { SourceKind::HeldConnection }
    
    async fn next(&mut self) -> Result<Option<ExternalEvent>> {
        // Socket Mode keeps a held connection open; next() returns events as they arrive
        let slack_event = self.client.next_event().await?;
        
        let event = ExternalEvent {
            source: "slack".to_string(),
            delivery_id: slack_event.envelope_id,
            kind: format!("slack.{}", slack_event.type_),
            data: serde_json::to_value(&slack_event)?,
            project_hint: slack_event.team_id.map(|t| format!("slack:{}", t)),
            delivered_at: Utc::now(),
        };
        
        Ok(Some(event))
    }
}
```

---

## Integration Points

### 1. trusty-agents-common (NEW: events seam)

Add to `crates/trusty-agents-common/src/events/`:
- `EventSource` trait + `SourceKind` enum (module: `sources.rs`)
- `ExternalEvent` struct (module: `external.rs`)
- Extend `HarnessPayload::External { source, kind, data }` arm (update module `mod.rs`)
- Unit tests for normalization round-trips per source

### 2. trusty-mpm (NEW: webhook ingress + goal store)

Add to `crates/trusty-mpm/src/`:
- `adapters/` module tree: `github_webhook.rs`, `jira_webhook.rs`, etc.
- Extend `crates/trusty-mpm/src/core/sm/goals/` with `ExternalEventSource`, `Goal::from_external_event`, `SmGoalStore::upsert_dedup`
- Extend `crates/trusty-mpm/src/project/resolver.rs` with `resolve_from_event()`
- New HTTP handlers in `daemon/api.rs`: `POST /api/webhooks/{source}` (mounted beside `/hooks`)
- Credential store & secrets (reuse trusty-common auth module, issue #550)

### 3. trusty-console (NEW: Tailscale Funnel binding)

Extend `crates/trusty-console/src/`:
- Add `BindMode::Funnel { tailnet_name: Option<String> }` variant (update `bind.rs`)
- CLI flag `--funnel` to enable public Tailscale Funnel ingress (default when internet-facing)
- Health check: verify Funnel is accessible via `curl -k https://your-funnel-domain/health`

### 4. trusty-agents (UPDATE: publish Socket Mode / long-poll events to bus)

Extend `crates/trusty-agents/src/`:
- Slack adapter: implement `EventSource` trait; publish received Socket Mode events to `HarnessEvent::publish_external()`
- Telegram adapter: implement `EventSource` trait; publish received long-poll messages to bus
- Gmail: Phase 1 (implement IMAP IDLE adapter); Phase 2 (Google Pub/Sub adapter)

### 5. trusty-mpm Project Registry (NEW: schema & resolver)

Issue #3091: Design the project registry schema to support per-project GitHub org/repo, JIRA project key, Slack workspace/channel, Telegram chat list, Gmail account, Calendar subscription list. The resolver (issue #3090) uses this to bind external events to projects.

---

## Relation to Existing Work

- **Supersedes #2084** (poll-based `/tm-issues-review` plan) — push-based ingestion is strictly superior (lower latency, lower resource cost) and now the foundation
- **Provides transport for #3065** (inbound-routing decision, Eve §10.7) — events ingested here are the input feed for routing logic
- **Foundation for SM integration #2109** (tm-manager epic) — goals created here are spawned into sessions by the SM
- **Cites ADR-0005** (event bus) — all external events flow through the shared `HarnessEvent` bus with the new `External` payload arm
- **Deferred: trusty-mcp-service adapters (#2733, DOC-27)** — concrete adapter implementations will eventually migrate to trusty-mcp-service when it materializes; this spec defines the contract they conform to

---

## Acceptance Criteria

- [x] EventSource contract defined in trusty-agents-common
- [x] ExternalEvent struct and HarnessPayload::External arm defined
- [x] Shared webhook ingress (/api/webhooks/{source}) specified with per-source routing
- [x] Signature verification requirements defined for GitHub, JIRA, Slack, Telegram, Google, Calendar
- [x] Goal lifecycle and idempotency model specified
- [x] Per-source normalization examples (GitHub, JIRA, Slack, Telegram) provided
- [x] Project resolution & registry integration specified
- [x] Credential storage (DOC-45 model) integrated
- [x] Integration points (crates, modules, traits) identified
- [x] Related specs and ADRs cross-referenced
- [x] No Rust code implemented (spec-only; implementation is follow-up)

---

## Follow-Up Work

**Implementation phases (tracked separately, see epic #3092):**

1. **Phase 1: EventSource + External payload arm** (issue #3090)
   - Add EventSource trait, ExternalEvent struct, HarnessPayload::External to trusty-agents-common
   - Unit tests for bus integration
   - Estimate: 2–3 days

2. **Phase 2: GitHub App webhook receiver + Funnel binding** (issue #3093)
   - Implement /api/webhooks/github with X-Hub-Signature-256 verification
   - Add Tailscale Funnel mode to trusty-console
   - Goal store integration (create, deduplicate)
   - Estimate: 3–4 days

3. **Phase 3: JIRA automation webhook receiver** (issue #3094)
   - Implement /api/webhooks/jira with X-Atlassian-Signature verification
   - Support automation-rules path (zero install)
   - Estimate: 2–3 days

4. **Phase 4: Connector bus publishing** (issue #3095)
   - Slack Socket Mode → HarnessEvent bus publishing
   - Telegram long-poll → HarnessEvent bus publishing
   - Gmail IMAP IDLE adapter (Phase 1)
   - Estimate: 3–5 days

5. **Phase 5+: Calendar, optional webhooks (Slack/Telegram), Cloud Pub/Sub**
   - Spring-2027 roadmap; lower priority

---

## Appendix: Glossary

- **EventSource:** A trait defining a source of external events (push webhook, held connection, poll fallback). Adapters implement this to emit ExternalEvent objects to the HarnessEvent bus.
- **HarnessPayload::External:** A first-class arm of the HarnessPayload enum (alongside Lifecycle, Hook, Ping) carrying source, kind, and normalized event data.
- **ExternalEvent:** The normalized shape of an external event before publishing to the bus; includes source name, delivery ID, kind, data, project hint, and delivered_at timestamp.
- **Goal:** A durable unit of work (from an external event) stored in the SM's goal store. Goals seed sessions; they are not notifications.
- **SmGoalStore:** The session manager's persistent store for goals. Provides dedup semantics (upsert_dedup) so re-ingestion of the same delivery ID updates the existing goal.
- **Tailscale Funnel:** A Tailscale feature providing public HTTP(S) ingress to a tailnet device without inbound firewall rules; uses Tailscale's regional relay servers.
- **SourceKind:** An enum (Push, HeldConnection, Poll) classifying a source's delivery mechanism for transparent fallback and diagnostics.

---

## Spec Version & Changelog

**~draft (current)**
- Initial draft (2026-07-19)
- Defined EventSource contract, External payload arm, shared webhook ingress, signature verification, goal lifecycle, per-source matrix
- Identified integration points and follow-up phases
