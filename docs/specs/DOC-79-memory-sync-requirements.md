# DOC-79 — Shared Engineering-Project Memory Sync: Requirements

**Status:** Draft (requirements only; no runtime implementation authorized)
**Spec ID:** `SPEC-MEMSYNC-REQ-01~draft` … `SPEC-MEMSYNC-REQ-15~draft` (DOC-79)
**Subsystem:** `trusty-memory` (sync connector, dream worker, local ingest boundary); `trusty-common` (`memory_core` record model and dream passes, shared with in-flight runtime work); remote memory-store API (location decided by [ADR-0067](../adr/0067-remote-memory-store-api-spec-location.md))
**Owner:** Bob Matsuoka (rulings); runtime owner assigned by the Architect (implementation)
**Last-updated:** 2026-10-04 (r2: review findings, owner decisions on OQ-1 to OQ-4, dream worker scope)
**Builds on:** [`docs/reference/shared-memory-identity.md`](../reference/shared-memory-identity.md) (content-hash identity, [#5902](https://github.com/bobmatnyc/trusty-tools/issues/5902)); the closed vision issue [#1683](https://github.com/bobmatnyc/trusty-tools/issues/1683) (closed not-planned 2026-09-03)
**Design:** [DOC-80](./DOC-80-memory-sync-design.md) holds the API proposal, architecture, consistency model, dream worker design, decisions, open questions, staged plan and issue breakdown.
**DOC-N claim:** `DOC-79` and `DOC-80`, per `docs/specs/README.md` ("Next free `DOC-N` = `DOC-79`"). No tracked file on `origin/main` (e3ff4c7358) and no open PR names either number (checked 2026-10-04). Re-verify before merge, per [DOC-38 §4.1](./spec-linked-documentation.md).

---

## 0. How to read this document

Each requirement carries an ID (`R-<area>-<n>`), a level (MUST, SHOULD, MAY) and a source. Sources are one of:

| Source tag | Meaning |
|---|---|
| **Bob** + timestamp | An owner ruling from the 2026-10-04 brainstorming note or session brief. Binding. |
| **Bob decision** + date | An owner answer to an open question in DOC-80 §13, or a scope addition. Binding. |
| **Codex (proposal)** | A design proposal recorded in the note. Not a ruling; adopted here as a requirement only where stated, and open to Bob's override. |
| **Review r1** | A requirement added to close a gap found by the 2026-10-04 code-analyzer review of r1. |
| **Derived** | Follows from a ruling plus verified code facts in DOC-80 §2. |

Every claim about current code lives in [DOC-80 §2](./DOC-80-memory-sync-design.md), marked VERIFIED with `path:line` or PROPOSED. This document states what must be true, not how. Every requirement traces to at least one conformance case in §14; the table in §14.3 is the check.

Supporting research, dated 2026-10-04, is in `docs/specs/research/`: code internals, standards and GitHub auth, vendor interoperability, and repo conventions.

## 1. Scope {#SPEC-MEMSYNC-REQ-01~draft}

### 1.1 In scope

- Sharing **engineering project memory** across users and machines: several trusty-mpm sessions, run by different people on different machines, working on the same project (Bob, 22:27Z). Later: harness-managed cloud sessions and cloud agents using the same contract.
- One **interoperable memory-sharing API specification** with interchangeable backends, not one cloud product. Clients may subscribe to several endpoints: "a mesh with unique identifiers for the originators of memories" (Bob, 22:28Z).
- A **sync connector** in the `trusty-memory` crate that runs as a separate background process under the daemon (Bob, 22:29Z; OQ-1 decided 2026-10-04).
- The **dream cycle moved out of the daemon process** into a daemon-supervised child process under the same supervision model as the connector (Bob decision, 2026-10-04 scope addition).
- Requirements, a protocol/API proposal, an architecture and crate/repo map, open decisions, acceptance criteria and a staged plan (Bob, 22:31Z).

### 1.2 Out of scope

- General personal-assistant memory, personal profiles, and projectless or organization-global memory (Bob, scope correction 22:33Z).
- Bulk import of a whole vendor user profile (Bob, 22:33Z).
- Runtime implementation, deployment, cloud provisioning, GitHub App or OAuth app registration, and creation of a reference-implementation repository (Bob, 22:31Z and 22:34Z). Runtime implementation of the dream worker is also out of scope here; the Architect assigns it after the fixes for [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173).
- The position paper on the roles of memory, search, requirements, specs and tickets. It is on hold until the HyperDev article "Why memory is different" is written (Bob, 22:39Z). This document states the Addendum E content rules as requirements (§3) and goes no further.

## 2. Terms {#SPEC-MEMSYNC-REQ-02~draft}

| Term | Meaning in this spec |
|---|---|
| Project | One engineering project with a stable, machine-portable identity: a project UUID committed in the repository's pin file, plus per-provider bindings (OQ-2, decided 2026-10-04). Today the local unit is a palace; DOC-80 §2.4 shows the palace id is not portable. |
| Memory record | The unit that syncs: one memory, at one revision, with its envelope (DOC-80 §5). |
| Canonical artifact | A ticket, issue, PR, spec section, requirement, ADR or commit that is the authoritative home of a work item, its status, its priority or its formal definition. |
| Decision state | `tentative`, `decided` or `superseded`, carried on every memory record. |
| Originator | The authenticated principal (person or workload) that authored a record. |
| Endpoint | One remote memory store that implements the API. |
| Connector | The sync child process. It moves records between the local store and endpoints. It does not answer recall. |
| Dream worker | The dream child process. It decides consolidation, dedup and prune actions and asks the daemon to apply them. It does not answer recall. |
| Child | Either the connector or the dream worker. |
| Local recall | The existing trusty-memory recall path. |

## 3. Content boundary: memory does not duplicate artifacts {#SPEC-MEMSYNC-REQ-03~draft}

Source for this section: Bob, 22:36Z (Addendum E), the core memory requirement. The Codex refinements in the same note are marked.

### 3.1 Work-item field list

This is the one list of work-item fields. DOC-80 §5 refers to it and does not restate it. A shared record carries none of these, at any nesting depth, in any letter case: `status`, `state` (of a work item), `priority`, `severity`, `assignee`, `assignees`, `labels`, `milestone`, `acceptance_criteria`, `due`, `due_date`, `estimate`, `story_points`, `requirement_text`.

### 3.2 Requirements

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-CONT-1 | MUST | A shared memory record does not copy a ticket, spec, requirement or ADR, and carries no §3.1 field. Local ingest and every endpoint reject a record that (a) has a §3.1 field, or (b) has a content line that starts with a §3.1 field name followed by `:` (for example `status: blocked`), or (c) has a Markdown task-list block (`- [ ]` / `- [x]`) of two or more items. The publishing client also rejects (d) content that shares a verbatim run of 200 or more characters, after whitespace normalization, with the body of a linked artifact it can fetch. The endpoint is not required to fetch artifacts; (d) is a client obligation. | Bob 22:36Z; Review r1 (testable form) |
| R-CONT-2 | MUST | A record captures what artifacts lose: the user's expressed and evolving intent, rationale, transitional thinking, unresolved trade-offs, alternatives considered, conditions, and emerging decisions. | Bob 22:36Z |
| R-CONT-3 | MUST | A record links its canonical artifacts by reference (issue, PR, spec section with revision, ADR, commit) instead of copying their content. A reader gets current status and priority from the linked artifact, never from memory. | Bob 22:36Z; Codex (proposal) for "relevant versions as anchors" |
| R-CONT-4 | MUST | Every record carries a decision state: `tentative`, `decided` or `superseded`. | Bob 22:36Z |
| R-CONT-5 | MUST | A `superseded` record names the record or canonical artifact that superseded it. | Derived |
| R-CONT-6 | MUST | Between memory records on the same subject, a `tentative` record never outranks a `decided` one in recall ranking or in sync conflict handling. Recall output of a record with canonical refs names those refs, so status comes from the artifact (R-CONT-3); recall does not read artifacts. | Codex (proposal), adopted; Review r1 (scoped to records) |
| R-CONT-7 | MUST | A record distinguishes what the user expressed from what an assistant synthesized. Unexpressed private thoughts are never inferred or claimed. | Codex (proposal), adopted |
| R-CONT-8 | MUST | When intent becomes a ticket, spec, requirement or ADR, the record links the resulting artifact and records the transition. It keeps the historical rationale and does not keep a competing statement of the current requirement. | Codex (proposal), adopted |
| R-CONT-9 | SHOULD | Promotion policy values emerging context before it has high reuse counts. Frequency is not the only value signal. | Codex (proposal), adopted |
| R-CONT-10 | MUST | The schema (DOC-80 §5) and the conformance cases (§14) enforce R-CONT-1 to R-CONT-8 and R-CONT-11 to R-CONT-12, not only the documentation. Every object in the envelope, nested ones included, is closed. | Bob 22:36Z ("apply this to the requirements, the schema and the evals"); Review r1 (nested) |
| R-CONT-11 | MUST | A local drawer without an explicit `claim_kind`, `expression` and `decision_state` is unclassified and never sync-eligible. Only a user action or an explicit classifier the user enabled sets all three. No default sets `decided`. | Review r1 |
| R-CONT-12 | MUST | Decision-state transitions and `claim_kind` × `decision_state` × `expression` combinations follow DOC-80 §5.5. Local ingest and the endpoint reject any other. | Review r1 |

## 4. Project identity and isolation {#SPEC-MEMSYNC-REQ-04~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-PROJ-1 | MUST | Every memory record carries an explicit project identity. A record without one is rejected at every boundary. | Bob 22:33Z |
| R-PROJ-2 | MUST | Project identity is a project UUID committed in the repository's pin file, with per-provider bindings (for example `github:repo:<numeric id>`). It is stable across machines, checkout paths, worktrees, repository renames and transfers. A local path, a hostname, a palace id or a mutable slug is not a project identity. | Bob decision 2026-10-04 (OQ-2); Derived (DOC-80 §2.4) |
| R-PROJ-3 | MUST | A multi-project user and a multi-endpoint connector keep project boundaries: a record never moves from one project's partition to another's. | Bob 22:33Z |
| R-PROJ-4 | MUST | A palace with no project (for example an assistant-scoped palace, which ADR-0051 calls "inapplicable") is never sync-eligible. | Derived |
| R-PROJ-5 | MUST | Machine-local metadata (absolute paths, workstream names, local session ids, `creator:cwd=` and `ws:` tags) does not leave the machine unless a field is defined for it and the policy permits it. | Derived (DOC-80 §2.3) |
| R-PROJ-6 | MUST | The pin file supplies only a candidate project UUID. Sync for a project starts only after an explicit local bind of project, palace and endpoint by the user. The bind, and every connector start, is refused unless the endpoint's project binding names the same repository that the checkout's remote resolves to. | Review r1 safeguard on Bob decision 2026-10-04 (OQ-2) |
| R-PROJ-7 | MUST | Sync identity stays out of `palace.json`. It lives in the sync binding and the envelope; ADR-0051 is amended afterwards. | Bob decision 2026-10-04 (OQ-3) |

## 5. Sync direction and the connector boundary {#SPEC-MEMSYNC-REQ-05~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-SYNC-1 | MUST | The connector is sync-only. It is not a recall, query, inference or summarization service. | Bob 22:31Z |
| R-SYNC-2 | MUST | Sync is bidirectional: outbound publication of eligible local records and inbound import of authorized remote records. "Sync-only" does not mean download-only. | Bob 22:31Z ("do not infer that 'sync-only' means download-only"); resolved here |
| R-SYNC-3 | MUST | All recall uses the existing local recall mechanism. Callers do not choose a cloud recall path. No synchronous remote fetch is inserted into recall. | Bob 22:29Z |
| R-SYNC-4 | MUST | Imported records become recallable only after successful local ingestion, through the same indexes normal recall uses. | Bob 22:29Z |
| R-SYNC-5 | MUST | The connector writes to the local store only through a supported daemon write interface. It never opens palace storage files itself. | Codex (proposal), adopted; also forced by the store lock (DOC-80 §2.2) |
| R-SYNC-6 | MUST | Imported and local records stay distinguishable in storage and in recall results. Recall marks imported records, marks conflict-set members, and excludes quarantined records. | Codex (proposal), adopted; Review r1 (recall markers) |
| R-SYNC-7 | MUST | Explicit authoritative project decisions can be published without waiting for a dreaming cycle. | Codex (proposal), adopted |
| R-SYNC-8 | MUST | The daemon accepts sync and dream methods only from the child it spawned, proven by a secret handed to the child at spawn. Any other caller, same UID included, is refused. | Review r1 |

## 6. Sharing eligibility and promotion {#SPEC-MEMSYNC-REQ-06~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-ELIG-1 | MUST | Sharing eligibility (may this record leave the machine, and to which project collection) and quality/promotion (is it worth sharing) are separate decisions with separate inputs. | Bob 22:28Z agreed with Codex |
| R-ELIG-2 | MUST | A record is private by default. It becomes eligible only by an explicit project policy or an explicit user action. | Derived |
| R-ELIG-3 | MUST | Dreaming or any other process may nominate, consolidate and flag contradictions. It cannot widen sharing permission, and it cannot turn repeated copies of an assertion into independent evidence. | Codex (proposal), adopted |
| R-ELIG-4 | MUST | Sharing a record requires permission for the supporting content it links or quotes. A record that quotes or derives from a record not eligible for the same project is held. | Codex (proposal), adopted |
| R-ELIG-5 | MUST | A consolidation or synthesis is eligible only if every `derived_from` source is eligible for the same project. | Review r1 (F12) |

## 7. Identity and provenance {#SPEC-MEMSYNC-REQ-07~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-ID-1 | MUST | Originator identity, memory identity, revision identity and endpoint identity are four separate identifiers. | Codex (proposal), adopted |
| R-ID-2 | MUST | A memory replicated to several endpoints keeps its memory id and its original attribution. Moving it to a new endpoint does not make it a new memory. | Codex (proposal), adopted |
| R-ID-3 | MUST | The originator of a record is bound by authentication at the endpoint. A self-declared author tag is not sufficient. | Codex (proposal), adopted |
| R-ID-4 | MUST | Author, editor, publisher, host and replicator are recorded as distinct roles. Consolidation and import preserve the original author. | Codex (proposal), adopted |
| R-ID-5 | MUST | Principal identifiers are stable provider-qualified ids, never mutable usernames or repository slugs. | Codex (proposal), adopted; GitHub docs confirm user `login` is mutable (research, standards-auth §3) |
| R-ID-6 | MUST | A cloud workload or agent is a principal distinct from the human it acts for, and the record says on whose behalf it acted. | Codex (proposal), adopted |
| R-ID-7 | MUST | Local ingest recomputes `revision_id` and rejects a mismatch. It records which endpoint attested the author, and accepts author attestation only from the project's authority endpoint; other attestations are kept as unattested. | Review r1 |

## 8. Change semantics {#SPEC-MEMSYNC-REQ-08~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-CHG-1 | MUST | Updates, supersessions, retractions and deletions travel as explicit events. | Bob note, suggested flow step 5 |
| R-CHG-2 | MUST | Retries do not duplicate records. Duplicate and out-of-order delivery converge to the same local state. | Bob note, step 5 |
| R-CHG-3 | MUST | A connector never re-publishes an imported record to its own or another endpoint unless an explicit resharing policy allows it. No republishing loops. | Codex (proposal), adopted |
| R-CHG-4 | MUST | Offline machines stay fully usable and reconcile on reconnect. | Bob note, step 5 |
| R-CHG-5 | MUST | A delete purges the body on every replica that receives it; a tombstone keeps ids, time, reason and issuer for a published retention window so late or offline replicas learn of it. | Derived (research, standards-auth §5) |
| R-CHG-6 | MUST | A local user forget of a published record that the user authored produces a delete event. | Derived (DOC-80 §2.3: today a user forget leaves no durable record) |
| R-CHG-7 | MUST | A local forget of an imported record writes a local suppression marker. It never emits a delete for another principal's record, and a resync does not re-import the suppressed record. | Review r1 (F10) |
| R-CHG-8 | MUST | A replica whose cursor is older than the tombstone retention window receives a `cursor_expired` answer, re-lists the project and reconciles: every local copy that is tombstoned or absent at the endpoint is purged. A deleted record never comes back. | Review r1 (F4) |

## 9. Conflicts {#SPEC-MEMSYNC-REQ-09~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-CONF-1 | MUST | Concurrent conflicting revisions coexist with their evidence and applicability until an explicit later revision reconciles them. | Bob note, correctness questions |
| R-CONF-2 | MUST | Newest timestamp alone is never the authority rule. Recency does not establish truth. | Bob note; Codex (proposal) |
| R-CONF-3 | MUST | A correction links the record it supersedes. | Bob note |
| R-CONF-4 | MUST | The spec defines who may issue an authoritative correction or retraction of another principal's record, and both boundaries enforce it. The policy itself is open question OQ-7 in DOC-80. | Codex (proposal) |
| R-CONF-5 | MUST | A `conflict_recorded` outcome is surfaced: counted per project in status and marked in recall output. It is never treated as a silent success. | Review r1 (F6) |

## 10. Authentication, authorization and revocation {#SPEC-MEMSYNC-REQ-10~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-AUTH-1 | MUST | Access control is enforced at the endpoint (service boundary), not only by client-side tags. | Bob note, correctness questions |
| R-AUTH-2 | MUST | Authorization is per project and per operation (read, publish, correct, administer). | Derived |
| R-AUTH-3 | MUST | GitHub is evaluated as the preferred authentication option, not assumed. The API does not hard-code one identity provider. | Bob 22:34Z ("preferred option to evaluate") |
| R-AUTH-4 | MUST | Repository visibility alone never exposes project memory. A public repository does not make its memory public. | Bob note, GitHub additions |
| R-AUTH-5 | MUST | Personal provider tokens (for example a GitHub user token) never travel as memory content or metadata, and never reach a second endpoint. | Bob note, 22:34Z |
| R-AUTH-6 | MUST | A token issued for one endpoint is not usable at another (audience restriction). A client using several authorization servers defends against mix-up attacks. | Derived (RFC 9700, RFC 8707; research, standards-auth §6) |
| R-AUTH-7 | MUST | Access revocation has a defined local cache policy. The spec states plainly that revocation cannot retroactively unsee content already read or copied. | Bob note, step 5 |
| R-AUTH-8 | MUST | The spec publishes an upper bound on revocation latency (token lifetime plus reconciliation interval). | Derived |
| R-AUTH-9 | MUST | The daemon keeps a per-project access lease. If no successful authorization check for a project happens within the lease period (whether the connector is stopped, given up or offline), the daemon quarantines that project's imported records. The lease period is configured and published in status. | Review r1 (F5) |
| R-AUTH-10 | MUST | A 401 triggers a token refresh and a retry, never revocation handling. Only a 403 or an `access.revoked` event starts revocation handling. A credential refresh is atomic: a crash mid-refresh leaves either the old or the new credential, and a failed refresh leaves a re-authentication path. | Review r1 (F5; refresh LOW) |
| R-AUTH-11 | MUST | The client takes authorization servers from a list configured out of band, never from endpoint discovery alone. Endpoints and authorization servers use HTTPS; plain-HTTP loopback is allowed only in the sandbox harness. | Review r1 |
| R-AUTH-12 | MUST | A workload principal is bound to a specific workflow (for example GitHub Actions `job_workflow_ref` or a protected environment), not only to a repository or owner. A workload publishes only `agent_observation` records and never sets `decided`. | Review r1 |

## 11. Safety {#SPEC-MEMSYNC-REQ-11~draft}

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-SAFE-1 | MUST | Outbound records pass a secret gate before they leave the machine. A hit blocks the record; it is never silently redacted and sent. | Derived (DOC-80 §2.5 names the gap in today's export path) |
| R-SAFE-2 | MUST | Inbound records pass the local secret gate before ingestion. | Derived |
| R-SAFE-3 | MUST | Shared memory text is data. It never becomes an executable instruction and never confers another user's authorization. Recall output marks imported text as untrusted data and shows its provenance. | Bob note, correctness questions; Review r1 (recall output) |
| R-SAFE-4 | MUST | An outbox entry blocked by the secret gate is held, not dropped, together with every later revision of the same memory that has it as an ancestor. Held entries are counted in status. | Review r1 (F8) |

## 12. Process isolation, failure behavior and freshness {#SPEC-MEMSYNC-REQ-12~draft}

### 12.1 Process

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-PROC-1 | MUST | The connector runs as a separate OS process, supervised by the trusty-memory daemon, and is not reduced to an in-process async task. | Bob 22:29Z ("do not silently reduce it to an in-process async task"); Bob decision 2026-10-04 (OQ-1); [ADR-0066](../adr/0066-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md) |
| R-PROC-2 | MUST | The daemon supervises each child's lifecycle with one model: restart after an exit the daemon did not request, exponential backoff, a give-up state after a bounded number of restarts, health in status, and exit of the child when the daemon dies. | Codex (proposal), adopted; Bob decision 2026-10-04 (dream scope) |
| R-PROC-3 | MUST | A slow or unreachable endpoint, or a stopped or crashing connector, delays freshness only. It never blocks local recall or freezes the daemon. | Bob 22:29Z |
| R-PROC-4 | MUST | Last successful sync and pending change counts are tracked per project and per endpoint, and exposed through status metadata, not through a new recall interface. | Bob 22:29Z |
| R-PROC-5 | MUST | Dreaming and sync keep distinct responsibilities: dreaming evaluates and consolidates eligible knowledge; sync transports permitted changes and keeps provenance. Neither child may call the other's daemon methods. | Codex (proposal), adopted |
| R-PROC-6 | SHOULD | Sync can be disabled per machine and per project without affecting any other trusty-memory function. | Derived |

### 12.2 Failure rules (fail-closed)

Each rule closes one fail-open path found in review r1 (finding id in the source column).

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-FAIL-1 | MUST | The outbox entry for an eligible change is written in the same durable storage transaction as the drawer change. If the entry cannot be written, the change fails. A reconciliation scan compares eligible drawer ids with outbox and published state and reports any gap. | Review r1 (F1) |
| R-FAIL-2 | MUST | The connector classifies a publish failure by the problem detail's `retryable` member, not by status class. Retryable outcomes (including 401 after refresh, 408, 429, 5xx) stay pending. Final rejections become dead letters: kept, counted and shown in status and in freshness. No entry is acked without a final outcome. | Review r1 (F2) |
| R-FAIL-3 | MUST | An inbound event with an unknown `schema_version` or `kind` is parked as `blocked_unknown_schema`; the cursor does not advance past it. Every inbound drop is counted by reason in status. | Review r1 (F3) |
| R-FAIL-4 | MUST | `memory.sync_ingest` returns `applied` only after the record is durable in every index lane recall uses, or has a durable backfill entry for a lagging lane. No lane enqueue drops an ingested record. The connector's cursor never runs ahead of durable local state. | Review r1 (F7 and atomic-visibility finding) |
| R-FAIL-5 | MUST | The daemon derives staleness itself from the age of the last connector report and from connector liveness. A dead connector shows stale even if its last report said fresh. | Review r1 (F9) |
| R-FAIL-6 | MUST | A purge (delete tombstone, revocation purge, operator purge) removes the record from the drawer store, the vector, BM25 and closet indexes, KG triples it originated, the outbox, the connector's retry and dead-letter queues and the quarantine. Local consolidations that list it in `derived_from` are recomputed without it or purged. | Review r1 (F11) |

## 13. Dream worker {#SPEC-MEMSYNC-REQ-15~draft}

Source for this section: Bob decision 2026-10-04 (the dream cycle moves out of the daemon process into a daemon-supervised child), plus the data-integrity defect [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and the related removal-trail defect [#8729](https://github.com/bobmatnyc/trusty-tools/issues/8729).

| ID | Level | Requirement | Source |
|---|---|---|---|
| R-DREAM-1 | MUST | Dream passes run in a separate OS process, spawned only by the daemon holding the maintenance lease and supervised under R-PROC-2. One data root has at most one dream worker. | Bob decision 2026-10-04 |
| R-DREAM-2 | MUST | The dream worker reads and changes palace state only through authenticated daemon methods (R-SYNC-8). It never opens palace storage files. | Derived (store lock, DOC-80 §2.2) |
| R-DREAM-3 | MUST | The daemon applies each dream action (merge, prune, content prune, consolidation) as one atomic unit: every store change of the action and its maintenance-journal record become durable together, or none does. A worker crash at any point persists no partial merge and no unrecorded removal. | Bob decision 2026-10-04; [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) |
| R-DREAM-4 | MUST | A merge's combined content is durable when the apply returns; it survives a daemon restart. Every dream removal writes a maintenance-journal record. | [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) |
| R-DREAM-5 | MUST | Recall, remember, forget and sync keep working, within the recall baseline, while the dream worker is down, crashing or given up. A dream failure delays consolidation only. | Bob decision 2026-10-04; Derived (R-PROC-3) |
| R-DREAM-6 | MUST | The daemon refuses any dream action that sets or widens sync eligibility, changes a project binding, or moves a record toward `decided`. A consolidation of sources inherits eligibility only under R-ELIG-5. | Derived (R-ELIG-3) |
| R-DREAM-7 | MUST | The dream worker never merges, prunes or rewrites an imported record. A near-duplicate of an imported record is linked, not merged. | Derived (R-SYNC-6); related [#8729](https://github.com/bobmatnyc/trusty-tools/issues/8729) |
| R-DREAM-8 | MUST | Crash, restart and shutdown follow R-PROC-2. On daemon shutdown the worker stops within a grace period; an action the daemon has not applied is discarded, and the interrupted pass is recorded in dream status. | Bob decision 2026-10-04 |
| R-DREAM-9 | MUST | Every dream action carries preconditions on the drawers it touches (content hash and presence). The daemon refuses an action whose preconditions no longer hold, so a decision made on an old snapshot never overwrites a newer write. | Derived |
| R-DREAM-10 | MUST | Manual triggers (`memory.dream_run`, `dream_consolidate_room`) go to the dream worker. When it is down they return an "unavailable" error; the daemon never runs a dream pass in-process as a fallback. | Derived (Bob 22:29Z, "do not silently reduce it to an in-process async task") |
| R-DREAM-11 | SHOULD | `TRUSTY_DREAM_DISABLED` keeps its meaning: when set, no dream worker is spawned. | Derived (DOC-80 §2.1) |

## 14. Acceptance criteria and conformance cases {#SPEC-MEMSYNC-REQ-13~draft}

The connector, the dream worker, the local ingest boundary and every endpoint implementation pass this suite before a release claims conformance. Each case names the requirements it proves. Cases marked **error arm** prove a fail-closed rule. The suite mechanics (harness, fake endpoint, fixtures) and the stage in which each case first runs are in DOC-80 §12 and §14.

Baseline topology, unless a case says otherwise: one project P, two users (U1, U2) on two machines (M1, M2), one endpoint E1. Where a case needs two endpoints, the fake endpoint runs two instances (E1, E2).

### 14.1 Sync and content cases

| Case | Setup and action | Pass condition | Proves |
|---|---|---|---|
| C-01 Two-user round trip | U1 on M1 writes an eligible `tentative` record; both connectors run. | U2's local recall on M2 returns it, attributed to U1, marked imported, with the same memory id and revision id. | R-SYNC-2, R-SYNC-4, R-SYNC-6, R-ID-2 |
| C-02 Private stays private | U1 writes a record with no eligibility. | No endpoint receives it; the outbox holds no entry for it, whatever its quality score. | R-ELIG-1, R-ELIG-2 |
| C-03 Work-item field refused | A record carries a §3.1 field at the top level or inside a `canonical_refs` entry. | Rejected locally and by the endpoint with a schema error; nothing stored. | R-CONT-1, R-CONT-10 |
| C-04 Missing decision state refused | A record omits `decision_state`; separately a `superseded` record omits `superseded_by`; separately a correction omits the link to what it corrects. | Each is rejected at both boundaries. | R-CONT-4, R-CONT-5, R-CONF-3 |
| C-05 Link, do not copy | U1 records the rationale for a choice tracked in issue I and spec section S. | The record holds refs to I and S at a pinned revision; its content holds no copy of the issue status or the spec text; U2 resolves current status from I. | R-CONT-3 |
| C-06 Tentative never outranks decided | M2 holds a `decided` record D and receives a later `tentative` record T on the same subject. | Recall ranks D above T, labels T tentative and lists the canonical refs of both; conflict handling keeps D current. | R-CONT-6 |
| C-07 Transition recorded | A `tentative` memory is promoted when ADR-X is accepted. | A new head revision of the same memory has `decision_state = decided` and a `decision_recorded_in` ref to ADR-X; the old revision stays as history in `parents` and is not current; no record restates ADR-X's decision text as a current requirement. | R-CONT-8, R-CONT-12 |
| C-08 Synthesis labelled | An assistant summarises a user discussion into a record. | `expression = assistant_synthesis`; `derived_from` lists the user-expressed records; the author is preserved. | R-CONT-7, R-ID-4 |
| C-09 Collaborator recovery eval | Given only U1's shared records plus the linked artifacts, U2 (or an evaluator agent) answers: why was the choice made, what remains unsettled, what would change it, and what is its current status. | The first three answers come from memory; the status answer comes from the linked artifact; scoring gives no credit for reproduced ticket text. | R-CONT-2, R-CONT-3, Bob 22:36Z eval rule |
| C-10 Offline divergence | M2 goes offline; U1 and U2 each write revisions of the same memory; M2 reconnects. | Both revisions survive as a conflict set with evidence; neither is dropped; no timestamp-only winner. | R-CHG-4, R-CONF-1, R-CONF-2 |
| C-11 Duplicate and out-of-order delivery | E1 delivers the same batch twice and delivers a supersession before the record it supersedes. | Local state is identical to in-order, once-only delivery. | R-CHG-2 |
| C-12 No republishing loop | M2 subscribes to E1 and E2 and imports a record from E1. | M2 never publishes it to E2 or back to E1 unless an explicit resharing policy is set. | R-CHG-3 |
| C-13 Delete and retraction propagation | U1 forgets a record U1 authored and published; separately retracts another. | Delete: every replica purges the body and keeps a tombstone. Retract: content kept for audit, excluded from recall as current. A replica offline for less than the retention window still learns both. | R-CHG-1, R-CHG-5, R-CHG-6 |
| C-14 Project access revocation | U2 loses access to P. | E1 refuses U2's pull and push at once; M2's connector stops for P at its next contact and applies the cache policy (default quarantine); revocation latency stays within the published bound. | R-AUTH-1, R-AUTH-7, R-AUTH-8 |
| C-15 Endpoint migration | P moves from E1 to E2. | Memory ids, revision ids and original attribution are unchanged on E2; M2 does not re-import them as new records. | R-ID-2 |
| C-16 Project isolation | U1 works on projects P and Q against one endpoint. | No record of P appears in Q's local store or partition, under any sync order. | R-PROJ-3 |
| C-17 Recall survives connector failure | Kill the connector; make E1 unreachable; stall E1 for longer than the daemon's RPC timeout. | Recall latency and results stay within the sync-disabled baseline; the daemon stays responsive; status shows per-project, per-endpoint freshness and pending counts. On restart, sync resumes from its cursor. | R-SYNC-3, R-PROC-3, R-PROC-4 |
| C-18 Secret gate both ways | A record containing a credential-shaped token is eligible; separately, E1 serves one. | Outbound: blocked locally, never sent. Inbound: refused by local ingest; the refusal shows in status. | R-SAFE-1, R-SAFE-2 |
| C-19 Forged originator | A client publishes a record claiming another principal as author. | E1 rejects it, or records the authenticated publisher and refuses the claimed author. | R-ID-3 |
| C-20 Token audience | A token issued for E1 is presented to E2. | E2 rejects it. | R-AUTH-6 |
| C-21 Public repo, private memory | P's repository is public; a user with no project membership requests memory. | Refused. | R-AUTH-4 |
| C-22 Shared text is inert | An imported record contains instruction-shaped text ("run X", "you are authorized to Y"). | It is stored as data; recall output marks it untrusted and shows its provenance; no tool call or permission change follows from ingestion or recall. | R-SAFE-3 |
| C-23 Project-less record (error arm) | A record with no `project_id` reaches local ingest; separately the endpoint. | Rejected at both. | R-PROJ-1 |
| C-24 Identity is path- and rename-stable | P is checked out at two paths, in a worktree, and after a repository rename; the palace slug differs between M1 and M2. | All resolve to the same `project_id`; sync binds all of them to P; `palace.json` is unchanged. | R-PROJ-2, R-PROJ-7 |
| C-25 Projectless palace (error arm) | An assistant-scoped palace holds a record a user marks for publication. | Refused; nothing reaches the outbox. | R-PROJ-4 |
| C-26 Machine-local tags stripped | A record carries `creator:cwd=/Users/...`, `ws:...` and an absolute-path tag. | The published envelope carries none of them. | R-PROJ-5 |
| C-27 Hostile pin file (error arm) | A cloned repository's pin file names the UUID of a victim project V. | No sync starts until the user binds explicitly; the bind is refused because E1's binding for V names a different repository than the checkout's remote. No record of the local palace leaves the machine. | R-PROJ-6 |
| C-28 Child boundary audit | During C-01 and a dream pass, audit open files and daemon calls of both children. | Neither child opens a file under a palace directory; the connector calls no recall, query or inference method; E1's recorder sees no query call. | R-SYNC-1, R-SYNC-5, R-DREAM-2 |
| C-29 Explicit decision without dream | Dream worker disabled; U1 explicitly publishes a `decided` record. | It reaches E1 within one sync interval. | R-SYNC-7 |
| C-30 Unauthenticated caller (error arm) | A same-UID process without the spawn secret calls `memory.sync_ingest`, `memory.sync_outbox_ack` and `memory.dream_apply`. | Every call refused; nothing applied or acked. | R-SYNC-8, R-DREAM-2 |
| C-31 Pasted artifact text (error arm) | Content has a `status: blocked` line; separately a three-item `- [ ]` list; separately a 250-character verbatim run from the body of linked issue I. | Each is blocked by the publishing client; the endpoint rejects the first two. | R-CONT-1 |
| C-32 Closed nested schema (error arm) | A `canonical_refs` entry has an extra member; a `uri` breaks the grammar or exceeds 512 bytes; `applicability` has an unknown key; a tag outside the allow-list (`status:blocked`). | Each rejected at both boundaries. | R-CONT-1, R-CONT-10 |
| C-33 Unclassified drawer (error arm) | A legacy drawer lacks `claim_kind`, `expression` and `decision_state`; project policy makes its type eligible. | Not eligible; no envelope is built; no default `decided` appears. | R-CONT-11 |
| C-34 Illegal transition (error arm) | A content revision follows a `superseded` head; an `open_question` is set `decided`; an `agent_observation` is set `decided`. | Each rejected at both boundaries. | R-CONT-12 |
| C-35 Emerging context nominated | A one-day-old `tentative` rationale with reuse count 0. | The promotion policy can nominate it; no rule requires a reuse threshold; nomination does not make it eligible. | R-CONT-9, R-ELIG-1 |
| C-36 Consolidation eligibility (error arm) | Consolidations from (a) all-eligible sources of P, (b) one private source, (c) a source of Q. | Only (a) is eligible. | R-ELIG-5 |
| C-37 Supporting content held (error arm) | An eligible record links or quotes a private record. | Held; not published. | R-ELIG-4 |
| C-38 Four identifiers | Publish, then migrate host, then edit. | Author principal, `memory_id`, `revision_id` and host endpoint are distinct values; changing one leaves the others unchanged. | R-ID-1 |
| C-39 Login rename | U1 renames their GitHub login. | Principal id and attribution of U1's records are unchanged. | R-ID-5 |
| C-40 Workload principal (error arm) | A CI workload publishes on behalf of U1; separately a token from another workflow ref publishes; separately the workload sets `decided`. | First: workload principal plus `on_behalf_of = U1`, `expression = agent_observation`. Second and third: refused. | R-ID-6, R-AUTH-12 |
| C-41 Inbound integrity (error arm) | E1 serves a record whose content does not match `revision_id`; separately a non-authority endpoint attests an author. | First rejected; second ingested as unattested. | R-ID-7 |
| C-42 Forget of an imported record (error arm) | U2 forgets U1's imported record, then forces a resync. | M2 writes a suppression marker; no delete event is emitted; the record is not re-imported; U1's copy and E1 are unchanged. | R-CHG-7 |
| C-43 Cursor expired (error arm) | M2 stays offline past the retention window; U1 deletes a record meanwhile. | E1 answers `cursor_expired`; M2 re-lists and purges the deleted record; it never returns to recall. | R-CHG-8 |
| C-44 Correction policy (error arm) | A principal the configured policy does not permit retracts and deletes U1's record; a permitted principal supersedes it. | First refused at both boundaries; second accepted and links the predecessor. Policy values follow OQ-7 once decided. | R-CONF-3, R-CONF-4 |
| C-45 Conflict surfaced | A publish returns `conflict_recorded`. | Status counts it for P; recall marks the conflict set. | R-CONF-5, R-SYNC-6 |
| C-46 Per-operation authorization (error arm) | A read-only member publishes. | Publish refused; reads succeed. | R-AUTH-2 |
| C-47 Provider token containment | U1 signs in with a GitHub user token, publishes to E1, subscribes to E2. | The token never appears in any envelope, header or body seen by E1's or E2's recorder after the token exchange. | R-AUTH-5 |
| C-48 Offline revocation lease (error arm) | Stop M2's connector; let the lease period pass. | The daemon quarantines P's imported records; recall excludes them; status shows why. | R-AUTH-9, R-SYNC-6 |
| C-49 401 versus 403 (error arm) | E1 answers 401; then 403; separately the connector is killed mid-refresh. | 401: refresh and retry, no quarantine. 403: revocation handling. Killed refresh: the credential store holds the old or the new credential and the next start can re-authenticate. | R-AUTH-10 |
| C-50 Authorization-server discovery (error arm) | E1's metadata names an authorization server not on the configured list; separately an `http://` endpoint outside the sandbox. | Both refused before any credential is sent. | R-AUTH-11 |
| C-51 Blocked entry and descendants (error arm) | Revision r1 is blocked by the secret gate; r2 has `parents = [r1]`. | Both held and counted; neither published. | R-SAFE-4 |
| C-52 Outbox atomicity (error arm) | Inject a failure into the outbox append of an eligible write. | The write fails and nothing is stored; the reconciliation scan reports no gap. | R-FAIL-1 |
| C-53 Publish failure classes (error arm) | E1 answers 408, 429, 503, and 422 with `retryable: false`. | The first three stay pending and retry; the 422 entry is a dead letter, counted and shown in freshness; nothing is acked without a final outcome. | R-FAIL-2 |
| C-54 Unknown schema parked (error arm) | E1 serves an event with a newer `schema_version` and one with an unknown `kind`. | Both parked as `blocked_unknown_schema`; the cursor stays before them; drop counters show the reason. After an upgrade, both apply. | R-FAIL-3 |
| C-55 Lane-durable ingest (error arm) | Fill the BM25 lane queue, then ingest a batch. | `applied` is returned only once the record is durable in every lane or has a durable backfill entry; after backfill, a BM25-only query finds it; the cursor never passes undurable records. | R-FAIL-4, R-SYNC-4 |
| C-56 Dead connector looks stale (error arm) | Kill the connector right after a fresh report and keep it from restarting. | After the staleness threshold, status shows stale. | R-FAIL-5 |
| C-57 Purge scope (error arm) | Delete a record that sits in every index, the outbox, a retry queue, quarantine, and a local consolidation's `derived_from`. | It is gone from each; the consolidation is recomputed without it or purged. | R-FAIL-6 |
| C-58 Child supervision (error arm) | For each child: SIGKILL it repeatedly; then kill the daemon. | Restarts with growing backoff; after the bound, status shows `given_up`; recall unaffected throughout; daemon death makes the child exit. | R-PROC-1, R-PROC-2, R-DREAM-1, R-DREAM-8 |
| C-59 Sync disabled per project | Disable sync for P; Q keeps syncing. | Q syncs; recall, remember, forget and dream on P are unaffected. | R-PROC-6 |
| C-60 Responsibility split (error arm) | The connector calls a dream method; the dream worker calls a sync method. | Both refused by caller identity. | R-PROC-5 |
| C-69 Provider-neutral authorization | Rerun C-01 with the endpoint's authorization server federating a non-GitHub test identity provider. | C-01 passes unchanged; no API field or path names GitHub. | R-AUTH-3 |

### 14.2 Dream worker cases

| Case | Setup and action | Pass condition | Proves |
|---|---|---|---|
| C-61 Crash mid-merge (error arm) | Kill the dream worker at each injection point of a dedup merge (before, during and after the apply call); then restart the daemon. | Either the survivor holds the merged text durably, the loser is gone and the journal holds its record, or none of the three changed. The merged text survives the daemon restart. | R-DREAM-3, R-DREAM-4 |
| C-62 Recall without dream (error arm) | Stop the dream worker; then drive it to `given_up`. | Recall, remember, forget and sync stay within baseline; dream status shows the state. | R-DREAM-5 |
| C-63 Dream cannot widen eligibility (error arm) | The worker submits actions that set eligibility, change a binding, and move a record to `decided`; it consolidates three copies of one private assertion. | Every widening action refused; the consolidation stays private and records one source, not three independent ones. | R-DREAM-6, R-ELIG-3 |
| C-64 Imported records untouched (error arm) | A dedup pair contains an imported record. | No merge, prune or rewrite of the imported record; a link is recorded instead. | R-DREAM-7 |
| C-65 Stale action refused (error arm) | After the worker's snapshot, the user edits one drawer and forgets another; the worker then submits actions on both. | Both refused as stale; nothing changes. | R-DREAM-9 |
| C-66 Shutdown mid-pass | Start daemon shutdown during a pass. | The worker exits within the grace period; no partial action is applied; dream status records the interrupted pass. | R-DREAM-8 |
| C-67 Manual trigger, worker down (error arm) | Call `memory.dream_run` and `dream_consolidate_room` while the worker is down. | Both return "unavailable"; no in-process pass runs. | R-DREAM-10 |
| C-68 One worker, or none | Two daemons share a data root; then set `TRUSTY_DREAM_DISABLED=1`. | Only the lease holder runs a dream worker; with the flag set, none is spawned. | R-DREAM-1, R-DREAM-11 |

Measurable targets (R-PROC-3, R-DREAM-5, C-17, C-62) are set in Stage 1 from a measured baseline, not invented here: local recall p50/p95 with sync disabled, the same with the connector running, end-to-end sync delay, and revocation latency. No target exists yet (Bob note: "High performance and low latency are design goals, not established measurements").

### 14.3 Traceability

Every requirement maps to at least one case.

| Requirement | Cases |
|---|---|
| R-CONT-1 | C-03, C-31, C-32 |
| R-CONT-2 | C-09 |
| R-CONT-3 | C-05, C-09 |
| R-CONT-4 | C-04 |
| R-CONT-5 | C-04 |
| R-CONT-6 | C-06 |
| R-CONT-7 | C-08 |
| R-CONT-8 | C-07 |
| R-CONT-9 | C-35 |
| R-CONT-10 | C-03, C-32 |
| R-CONT-11 | C-33 |
| R-CONT-12 | C-07, C-34 |
| R-PROJ-1 | C-23 |
| R-PROJ-2 | C-24 |
| R-PROJ-3 | C-16 |
| R-PROJ-4 | C-25 |
| R-PROJ-5 | C-26 |
| R-PROJ-6 | C-27 |
| R-PROJ-7 | C-24 (identity resolves with no `palace.json` change) |
| R-SYNC-1 | C-28 |
| R-SYNC-2 | C-01 |
| R-SYNC-3 | C-17 |
| R-SYNC-4 | C-01, C-55 |
| R-SYNC-5 | C-28 |
| R-SYNC-6 | C-01, C-45, C-48 |
| R-SYNC-7 | C-29 |
| R-SYNC-8 | C-30 |
| R-ELIG-1 | C-02, C-35 |
| R-ELIG-2 | C-02 |
| R-ELIG-3 | C-63 |
| R-ELIG-4 | C-37 |
| R-ELIG-5 | C-36 |
| R-ID-1 | C-38 |
| R-ID-2 | C-01, C-15 |
| R-ID-3 | C-19 |
| R-ID-4 | C-08 |
| R-ID-5 | C-39 |
| R-ID-6 | C-40 |
| R-ID-7 | C-41 |
| R-CHG-1 | C-13 |
| R-CHG-2 | C-11 |
| R-CHG-3 | C-12 |
| R-CHG-4 | C-10 |
| R-CHG-5 | C-13 |
| R-CHG-6 | C-13 |
| R-CHG-7 | C-42 |
| R-CHG-8 | C-43 |
| R-CONF-1 | C-10 |
| R-CONF-2 | C-10 |
| R-CONF-3 | C-04, C-44 |
| R-CONF-4 | C-44 |
| R-CONF-5 | C-45 |
| R-AUTH-1 | C-14 |
| R-AUTH-2 | C-46 |
| R-AUTH-3 | C-69 |
| R-AUTH-4 | C-21 |
| R-AUTH-5 | C-47 |
| R-AUTH-6 | C-20 |
| R-AUTH-7 | C-14 |
| R-AUTH-8 | C-14 |
| R-AUTH-9 | C-48 |
| R-AUTH-10 | C-49 |
| R-AUTH-11 | C-50 |
| R-AUTH-12 | C-40 |
| R-SAFE-1 | C-18 |
| R-SAFE-2 | C-18 |
| R-SAFE-3 | C-22 |
| R-SAFE-4 | C-51 |
| R-PROC-1 | C-58 |
| R-PROC-2 | C-58 |
| R-PROC-3 | C-17 |
| R-PROC-4 | C-17 |
| R-PROC-5 | C-60 |
| R-PROC-6 | C-59 |
| R-FAIL-1 | C-52 |
| R-FAIL-2 | C-53 |
| R-FAIL-3 | C-54 |
| R-FAIL-4 | C-55 |
| R-FAIL-5 | C-56 |
| R-FAIL-6 | C-57 |
| R-DREAM-1 | C-58, C-68 |
| R-DREAM-2 | C-28, C-30 |
| R-DREAM-3 | C-61 |
| R-DREAM-4 | C-61 |
| R-DREAM-5 | C-62 |
| R-DREAM-6 | C-63 |
| R-DREAM-7 | C-64 |
| R-DREAM-8 | C-58, C-66 |
| R-DREAM-9 | C-65 |
| R-DREAM-10 | C-67 |
| R-DREAM-11 | C-68 |

## 15. Ownership note {#SPEC-MEMSYNC-REQ-14~draft}

This section is a coordination note, not a requirement; it carries no conformance case. Runtime trusty-memory work in flight belongs to the runtime owner assigned by the Architect: the memory milestones M1 to M3, the f0 reclaim, the dreaming fixes [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172), [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) and [#9174](https://github.com/bobmatnyc/trusty-tools/issues/9174), installs, daemon restarts, releases, version bumps, and the shared `trusty-common` crate. A sync or dream-worker change that needs any of that work goes to the Architect. DOC-80 §11 lists each dependency.
