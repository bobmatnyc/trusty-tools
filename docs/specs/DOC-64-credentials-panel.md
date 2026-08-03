---
spec_refs:
  - id: SPEC-CREDAUTH-07~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-07~draft
  - id: SPEC-CREDAUTH-01~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-01~draft
  - id: SPEC-AGENTCFG-06~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-06~draft
---

# DOC-64 — The Credentials Panel: Per-Assistant Credential Sets, Transfer, and the User-Granted Copy

**Status:** Draft
**Spec ID:** `SPEC-CREDPANEL-01~draft` … `SPEC-CREDPANEL-09~draft` (DOC-64)
**Subsystem:** `trusty-agents` — the assistant configuration surface (panel, backing route, audited actions); `trusty-common` — the authority the panel is a **client** of. The panel owns no authority.
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-08-03
**DOC-N claim:** `DOC-64`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md), verified free four ways on `origin/main` (`99f085a3`): no filename or self-label claim under `docs/specs/**` or `docs/trusty-installer/research/02-design/**` (highest is `DOC-63`); no claim in any open PR (#4526, #4578, #4640, #4641 — none a spec), the check `scripts/check_doc_numbers.sh` structurally cannot make; no claim on any remote branch (`spec-twin-lead-architecture` claims `DOC-44` alone); `check_doc_numbers.sh` clean.
**Builds on:** [DOC-45](./DOC-45-credential-authority-model.md) — the authority (principals, grants, revocation, audit). [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md) — a grant does not survive delegation. [DOC-57](./agent-config-five-sections.md) — the configuration surface this panel joins. Decisions recorded in those and in #4040's owner comment of 2026-08-03 are **cited, not restated**.
**Related issues:** **#4663** (this spec); **#4040** (epic — the owner answers this encodes); **#4566** (principal + ACL — hard dependency); **#4567** (the audit stream — hard dependency); **#4667** (the DOC-45 audit-record amendment this panel's events require); #4565 (`CredentialRef`/`Secret`, landed `0c0de180`); #4632 (`SecretString`'s four-character disclosure — active hazard); #4570 (at-rest storage)

---

## 1. Summary

The owner's 2026-08-03 answers on #4040 made **segmentation** the goal — one
credential store per assistant instance — and ruled that an assistant may **ask**
for a credential another holds while **only the user grants**. This panel is
where the user does that; without it, "only the user grants" has nowhere to be
enforced.

The panel is a **client** of the authority DOC-45 ships, never a second
authority, and it **never displays a secret value**.

Five acts, each an audited event: view a set (§5), move a credential (§6),
approve or deny a copy request (§7), see provenance and last-use (§8), revoke
(§9).

**Two questions block building it** (§13): whether DOC-57's read-only rule
exempts this write path, and whether a "credential set" is a store or a grant
set. Everything else here is decided.

### 1.1 Verified current state — `origin/main` (`99f085a3`)

| Claim | Verified |
|---|---|
| `CredentialRef` and `Secret<T>` exist and are clean | `crates/trusty-common/src/credentials/{handle.rs:114,secret.rs:72}` (#4565, `0c0de180`). `Secret<T>`'s `Debug` (`secret.rs:106`) and `Display` (`:118`) are `impl<T>` with **no `T: Debug` bound** — the formatter structurally cannot read the value. Pinned by `debug_and_display_are_value_independent`. |
| `Principal::Assistant` does **not** exist | `credentials/principal.rs` carries `Operator` and `Service` only; the panel's whole subject arrives with #4566. `credentials/mod.rs:36` — *"This module holds no authorization."* |
| The config surface has six tabs, none for credentials | `crates/trusty-agents/ui/src/components/AgentConfigPanel.svelte:110` — `personality`, `knowledge`, `skills`, `subagents`, `listeners`, `permissions`. DOC-57 §8.2 specifies five; `subagents` came from #4029. No CLI surface is per-assistant: `tm agent` is list/show, and `tm`/`tagent config keys` is provider-global. |
| That surface has never had a credential write path | `AgentConfigPermissions.svelte:14` restates DOC-57 **PM-4** — granting from a GUI is a security-relevant write path, out of scope there. §13 OQ-1. |
| `SecretString` discloses four characters of the live value | `inference/types/secret.rs:80` via `redact_secret` (`credentials/redact.rs:43`). #4632, open, pinned by three tests. |

---

## 2. Scope

**In scope:** the surface — what a user sees of an instance's credential set, the
five acts, the event each emits, and the constraint that no act renders a value.

**Out of scope:** the authority itself (DOC-45), at-rest storage (#4570),
credential acquisition, the notification transport (#4646), and **any widening of
reach** — this document grants nothing and adds no name to any reachable-set
floor. It also answers none of the three sub-questions #4040's Q1 deferred (§12).

**Terms.** *Credential set* — what one instance's principal can resolve (the word
is ambiguous between store contents and grant set: §13 OQ-2). *Instance* — an
`AssistantInstanceId`; `izzie` and `cto-assistant` are two. *Move* — revoke from
A, issue to B.

RFC-2119 force throughout. Clauses are `P-<section>.<n>` and are the citable
unit; **BLOCKED** names the dependency a clause waits on.

---

## 3. SPEC-CREDPANEL-01 — What the Panel Is {#SPEC-CREDPANEL-01~draft}

**ID:** `SPEC-CREDPANEL-01~draft`
**Status:** Draft.

**P-1.1** The panel SHALL hold no authorization logic. Every act resolves to a call
the authority evaluates, records, and MAY refuse — a panel that decided anything
itself would be DOC-45 `C-3.3`'s forbidden second entry point.

**P-1.2** The panel SHALL act **as the Operator principal** and only as the
Operator (DOC-45 `C-3.6`). The one exception is `P-8.6`'s copy request, whose
actor is the requesting assistant.

**P-1.3** The panel SHALL NOT be reachable by an assistant as a tool. An
assistant's only means of affecting a credential set is §7's request.

**P-1.4 — BLOCKED on #4566.** The subject is one
`Principal::Assistant(AssistantInstanceId)` per instance. That variant does not
exist (§1.1), so nothing here is implementable before #4566.

**P-1.5** Two instances of the same persona type SHALL be separately displayed,
granted, and revoked.

**P-1.6** Segmentation SHALL have no escape hatch: every path by which a
credential reaches an instance that did not hold it — §6's move, §7's approved
copy — routes through the Operator and is recorded.

**Assumption an implementer may make (not an open question):** the panel is a
seventh tab in the existing GUI configuration surface, with a new
`AgentConfigCredentials.svelte` and a backing route — that is where every other
configuration section lives. A CLI equivalent is a follow-up; no clause here
depends on the surface.

---

## 4. SPEC-CREDPANEL-02 — The Panel Never Displays a Value {#SPEC-CREDPANEL-02~draft}

**ID:** `SPEC-CREDPANEL-02~draft`
**Status:** Draft. The hardest constraint, with a live counter-example in the tree.

**P-2.1** No surface of the panel — list row, detail view, tooltip, clipboard
action, error message, JSON response, log line, or panel-emitted audit record —
SHALL contain a credential value, a substring of one, or a prefix of one.

**P-2.2** The panel SHALL identify a credential by its `CredentialRef` and
metadata alone. The ref is non-secret by construction (DOC-45 `C-2.1`, `C-2.4`),
and `credentials/handle.rs` enforces the grammar that makes that safe.

**P-2.3** There SHALL be no reveal affordance and no read verb on any panel route
that returns a value — matching the deliberate absence of `get` from the existing
key CLI.

**P-2.4** `P-2.1` is achievable because `Secret<T>`'s `Debug`/`Display` are
value-independent **by construction** — `impl<T>` with no `T: Debug` bound
(§1.1), so the formatter cannot consult the value even if a later edit wanted it
to. That is the property this document depends on.

**P-2.5** No panel type SHALL hold a `Secret<T>`. The panel never calls
`resolve`, so it never has a value to leak.

**P-2.6** The panel SHALL NOT render, transport, or store `SecretString`, and
SHALL NOT use `redact_secret` — that path emits the **first four characters of
the live value** plus its exact byte length (#4632, open). A prohibition rather
than a preference, because a four-character head is what a designer reaches for
when rows need distinguishing and the tree already contains the function that
produces it. Rows are distinguished by the `CredentialRef` and nothing else.

**P-2.7** No row SHALL carry a value length, a hash of the value, or any field
derived from it. Both are oracles.

**P-2.8** A test SHALL assert that no byte of a stored credential appears in any
panel response, rendered view, or panel-emitted audit record.

---

## 5. SPEC-CREDPANEL-03 — View a Credential Set {#SPEC-CREDPANEL-03~draft}

**ID:** `SPEC-CREDPANEL-03~draft`
**Status:** Draft.

| Clause | Requirement |
|---|---|
| **P-3.1** | The panel SHALL list, per instance, one row per `CredentialRef` in that instance's set, carrying `credential_ref`, `provider`, `scope`, `state`, `expiry`, `provenance` (§8), `last_use` (§8), and `shared_with`. |
| **P-3.2** | `state` SHALL be **queried** from the authority (DOC-45 `C-6.2`), never inferred and never obtained by resolving — rendering twelve credentials must not resolve twelve secrets. |
| **P-3.3** | **Never fabricate.** Loading, empty, and error are three distinct rendered states (DOC-57 G-4). An empty list that is really a failed query reads as *"this assistant holds nothing"* — the most misleading thing this surface can say. |
| **P-3.4** | `shared_with` — the other instances holding a grant against the same ref — SHALL be displayed. Per-instance views in isolation would let a credential fan out across every assistant invisibly. |
| **P-3.5** | The panel is operator-visible and not model-visible, so it MAY render DOC-45 `C-5.10`'s full `Missing`/`Denied` distinction — but only because `P-1.3` holds, which an implementation MUST pin rather than assume. |

---

## 6. SPEC-CREDPANEL-04 — Move a Credential Between Assistants {#SPEC-CREDPANEL-04~draft}

**ID:** `SPEC-CREDPANEL-04~draft`
**Status:** Draft. Semantics depend on §13 OQ-2.

| Clause | Requirement |
|---|---|
| **P-4.1** | A move SHALL be **revoke-from-source then issue-to-target**, never a copy. On success the source no longer resolves the ref and the target does. |
| **P-4.2** | A move SHALL be atomic from the user's point of view. The dangerous failure is the partial move that issued to the target and failed to revoke the source: a silent copy that looks like success. |
| **P-4.3** | A move SHALL carry the source scope **unchanged or narrowed**, never widened — a move that could widen would be the cheapest way to escalate. |
| **P-4.4** | A move SHALL be refused with a stated, recoverable reason when the source holds no grant, the target already holds one, or the source grant is `Revoked` (DOC-45 `C-3.11`, `C-5.7`). Never a silent no-op. |
| **P-4.5** | A move SHALL NOT move the secret's bytes through the panel's process boundary, under either reading of §13 OQ-2. |
| **P-4.6** | A move SHALL NOT affect any sub-agent — §11. |

---

## 7. SPEC-CREDPANEL-05 — The Copy Request: An Assistant Asks, Only the User Grants {#SPEC-CREDPANEL-05~draft}

**ID:** `SPEC-CREDPANEL-05~draft`
**Status:** Draft. **Owner decision, 2026-08-03 (#4040) — settled.** Rationale is
recorded there and in [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md);
in one line, a copy that does not route through the user defeats segmentation via
its own escape hatch.

**P-5.1** A request SHALL be a durable object carrying `request_id`, requesting
principal, holding principal, `credential_ref`, requested scope, a reason, and a
creation instant.

**P-5.2** The reason SHALL be treated as **untrusted content** — model-authored,
shown to a user about to make a security decision, and therefore the
highest-value injection target on this surface. Rendered inert, never as markup
or a link, never interpolated into an instruction context, length-bounded.

**P-5.3** A request SHALL confer nothing while open, and nothing permanently if
denied or expired.

**P-5.4** A request SHALL be rate-bounded per `(requesting principal, ref)` —
unbounded requests are consent by attrition.

**P-5.5** Only the Operator SHALL resolve a request, and only through the panel —
no assistant, sub-agent, service, or automation, **including the holder**, which
never had transferable authority to consent with.

**P-5.6** Approval SHALL create a **new, independent grant** to the requester,
never an alias of the holder's; revoking either SHALL NOT affect the other
(DOC-45 `C-2.3`).

**P-5.7** The user MAY grant a narrower scope than requested and SHALL NOT be able
to grant a wider one here.

**P-5.8** Approval SHALL be per-request, never standing. A standing approval is
the assistant-to-assistant channel the owner rejected, expressed as a setting.

**P-5.9** Outcomes are three-valued — `approved`, `denied`, `timed_out`. A
timeout SHALL NOT be recorded as a denial.

**P-5.10** At decision time the panel SHALL display both principals, the ref, the
requested scope, the reason, the holder's current scope, and **what the
requester's set would look like afterwards**. The user is deciding about a
resulting reach, not a row.

**Assumptions an implementer may make (not open questions):** a pending request
waits in the panel until the user next opens it — reaching an absent user is epic
**#4646**'s problem, out of scope here; and a request surfaces under **both**
principals' panels, so it is not hidden behind whichever assistant was last
selected.

---

## 8. SPEC-CREDPANEL-06 — Provenance and Last-Use {#SPEC-CREDPANEL-06~draft}

**ID:** `SPEC-CREDPANEL-06~draft`
**Status:** Draft. Depends on #4567.

| Clause | Requirement |
|---|---|
| **P-6.1** | Each row SHALL display how this instance came to hold the credential: `issued_directly`, `moved_from <principal>`, `copy_approved_from <principal>`, or `provisioned`. |
| **P-6.2** | Provenance and last-use SHALL be **derived from the audit stream**, never a second ledger. A parallel record could disagree with the trail an incident review actually reads, and then the display is the lie. |
| **P-6.3** | Where the stream has rotated past the originating event, provenance SHALL render `unknown` explicitly — never `issued_directly` as a default. |
| **P-6.4** | The row SHALL show the most recent **allowed** resolution and, separately, the most recent **denied** attempt. Merged, the denial pattern becomes invisible. |
| **P-6.5** | A credential never resolved SHALL render `never used`, not an empty cell and not the grant's creation time — the most actionable row here, being a grant revocable at no cost. |
| **P-6.6** | Last-use SHALL be labelled as covering resolutions **through the authority**, not all use, while DOC-45 `C-3.12` holds — 55 raw `std::env::var` credential reads bypass the resolver today and leave no record. The caveat SHALL NOT be removed before #4571 lands. |

---

## 9. SPEC-CREDPANEL-07 — Revocation, and What It Means In Flight {#SPEC-CREDPANEL-07~draft}

**ID:** `SPEC-CREDPANEL-07~draft`
**Status:** Draft.

### 9.1 The act

| Clause | Requirement |
|---|---|
| **P-7.1** | Revoking SHALL revoke **one grant** — the `(Principal, CredentialRef)` pair the row names. It SHALL NOT delete the stored credential, revoke other principals' grants against the same ref, or contact the provider. Per-row revocation is what makes `P-1.5` operable. |
| **P-7.2** | The panel SHALL distinguish, in wording and confirmation, revoking a **grant** (what this surface does) from the credential being revoked **upstream** by the provider (DOC-45 `C-6.4`). |
| **P-7.3** | Revocation SHALL commit to authority state before the panel reports success. Optimistic success would tell a user their exposure had closed when it had not. |
| **P-7.8** | Revocation SHALL notify subscribers of the transition (DOC-45 `C-6.2`), so a consumer such as a scheduled OKG source stops rather than failing silently. |
| **P-7.9** | Revocation SHALL be reversible only by issuing a new grant, a separate act with its own record. An undo would leave a revocation in the trail that did not revoke anything. |

### 9.2 In flight — stated, not implied

**P-7.4** Revocation SHALL cause **the next resolution to fail**, before any
network call is attempted (DOC-45 `C-6.3`).

**P-7.5** Revocation SHALL NOT retract a `Secret` already resolved and in use. No
mechanism can reach into a call already issued or a subprocess already spawned
with the value in its environment; an in-flight operation completes.

**P-7.6** The window is bounded by **resolve-at-use-time** (DOC-45 `C-8.4`), so
exposure after a revoke is one operation long, not one session long. The panel
SHALL say so plainly rather than implying instantaneous cutoff: *revocation stops
the next use; an operation already running finishes.*

**P-7.7** Where that window matters, the remedy is **rotation at the provider**,
which the revoke confirmation SHALL name as the next step. Rotation does not
invalidate grants (DOC-45 `C-6.7`), so the two compose.

---

## 10. SPEC-CREDPANEL-08 — The Audited Events {#SPEC-CREDPANEL-08~draft}

**ID:** `SPEC-CREDPANEL-08~draft`
**Status:** Draft. **§10.2 is BLOCKED on #4667.**

Every panel action is an audited event on the single stream, per #4040's Q2
answer: one stream with a discriminator, records per resolution call.

### 10.1 The five events

**P-8.1** Panel events SHALL be emitted on the **same stream** as resolution
records, discriminated by category. Two streams would split *"the user moved
this"* from *"the target then resolved it"* across places a reader correlates by
hand.

**P-8.2** Panel events SHALL obey DOC-45 `C-7.3`–`C-7.5` without exception: the
record type accepts a `CredentialRef`, never a `String` that might hold a value;
no header, no URL query string, no partial credential.

**P-8.3** Panel events SHALL be independently suppressible from resolution
records — under one stream that means per-category filtering, not a single on/off
switch, since retaining low-volume administrative events while suppressing
high-volume resolutions is the configuration an operator will want.

Common envelope: `timestamp`, `stream_category`, `event_kind`, `actor`,
`session_id`, `call_site`.

| Clause | `event_kind` | Payload beyond the envelope | Note |
|---|---|---|---|
| **P-8.4** | `credential.view` | `subject`, `refs_listed` (a count, not the list) | — |
| **P-8.5** | `credential.move` | `from_principal`, `to_principal`, `credential_ref`, `scope_before`, `scope_after`, `outcome` (`committed`\|`refused`), `refusal_reason` | One event with two principals, not two events: a revoke plus an issue loses `P-4.2`'s atomicity and makes `P-6.1`'s `moved_from` unreconstructable |
| **P-8.6** | `credential.copy_requested` | `request_id`, `requesting_principal`, `holding_principal`, `credential_ref`, `requested_scope`, `reason` (bounded, inert) | `actor` is the requesting assistant — the one exception to `P-1.2` |
| **P-8.7** | `credential.copy_decided` | `request_id` (correlating to `P-8.6`), both principals, `credential_ref`, `granted_scope` on approval, three-valued `outcome` (`P-5.9`) | Without the correlation an approval cannot be tied to the reason that persuaded the user |
| **P-8.8** | `credential.revoked` | `subject`, `credential_ref`, `revocation_kind` (`grant` — the only kind here), `prior_state`, `in_flight_note` | `in_flight_note` records whether a resolution landed inside the declared window before the revoke committed, which is what makes `P-7.5` reviewable |

**P-8.9** `reason` SHALL be the only free-text field on any event, bounded at the
type level and stored and rendered inert (DOC-45 `C-7.5`).

**P-8.10** Every event SHALL be emitted whether the act succeeded or was refused.
A refused move and a denied copy are the more interesting forensic events, and
the ones a happy-path implementation drops.

### 10.2 The DOC-45 dependency

**P-8.11** The **per-resolution-call grain fits DOC-45's landed record
unchanged** — `C-7.7` already names it. Nothing in §10.1 asks to change the grain.

**P-8.16** DOC-45 `C-7.1`'s landed shape cannot carry `P-8.4`–`P-8.8`: no stream
discriminator, no event kind, one principal where a move needs two, no request
correlation id, and a two-valued decision axis. **The amendment is
[#4667](https://github.com/bobmatnyc/trusty-tools/issues/4667)**, which holds the
full analysis. This document does **not** perform it — #4563 is closed and DOC-45
shipped, and a second document quietly assuming a different shape would leave two
specs disagreeing about the one artifact an incident review reads. Until #4667
lands, `P-8.4`–`P-8.10` are **BLOCKED**; they are specified so the amendment has
a concrete requirement to satisfy.

---

## 11. SPEC-CREDPANEL-09 — What a Sub-Agent Gets: Nothing {#SPEC-CREDPANEL-09~draft}

**ID:** `SPEC-CREDPANEL-09~draft`
**Status:** Draft. Per [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md).

**P-9.1** No act on this panel SHALL confer anything on a sub-agent. Granting to
`Assistant(izzie)` grants nothing to
`SubAgent { name: "version-control", delegator: izzie }`, a distinct principal
resolving against its own grant, fail-closed (DOC-45 `C-4.2`, `C-4.3`).

**P-9.2** A sub-agent SHALL be shown with its own credential set or an explicit
empty one, never implied to share its delegator's.

**P-9.3** A sub-agent SHALL NOT be able to file a copy request against its
delegator's set — that would reconstruct inheritance as a one-click approval,
which is what ADR-0026 rejected.

---

## 12. The Deferred Q1 Sub-Questions

**P-10.1** No clause here answers any of the three sub-questions #4040's Q1
deferred; an implementation needing one MUST escalate. If they resolve: a
privileged `user.*` namespace (#3076) needs a carve-out in `P-4.1`/`P-5.6` for
whether such a credential is movable or copyable at all; a headless/SSH hard-fail
adds a rendered state to `P-3.3`; encrypting the file store touches nothing here,
since the panel never touches storage (`P-4.5`).

---

## 13. Open Questions for the Owner

Two, and only two, genuinely block building the panel. What was unresolved but
non-blocking is now a stated assumption in §3 and §7 — a wrong guess there costs
rework, not a rebuild.

### OQ-1 — Does DOC-57's PM-4 read-only rule exempt this panel?

DOC-57 **PM-4** states Permissions is read-only in every phase because *"granting
capability from a GUI is a security-relevant write path; it requires its own
review"*, and **C-06.4** is *"no route in this spec grants or widens a
permission"*. This panel is exactly that write path: the owner placed the copy
approval in it, so it cannot be read-only and still discharge #4663.

| Option | Cost |
|---|---|
| This spec plus its PR review **is** the review PM-4 anticipated | Nothing further; build proceeds. |
| A per-write operator confirmation gate around the panel | One extra dialog and its record. Composes with DOC-45 `C-10.6`, which already requires confirmation on first use of a credential. |
| A `user_authority` check (DOC-41 §5.5 / #3074) | Blocks the panel indefinitely — #3074 is unimplemented. |

### OQ-2 — Is a "credential set" a store, or a grant set?

#4040's Q3 says **store** (*"one credential store per assistant instance"*).
DOC-45 `C-2.3` models **grants** — one stored credential, several principals
separately granted against it. #4566 builds to one of them, so a wrong guess
wastes its principal and grant model and §6's move semantics.

| Option | Cost |
|---|---|
| **Grant set** — one stored credential, N grants | §6's move rewrites two grant rows; `shared_with` is a lookup; DOC-45 `C-2.2` (ref stable across rotation) and `C-6.7` stay true. Departs from the Q3 wording. |
| **Store** — N stores, N copies of the bytes | Matches the Q3 wording. Rotation must reach every store holding the credential or `C-2.2` breaks for all but one instance; `shared_with` becomes a cross-store comparison. |
| **Both** — one physical store, per-instance grant partitions presented *as* per-instance stores | Reconciles them, at the cost of a presentation layer. A product decision about what the user is told they own, not an implementation detail. |

---

## 14. Amendments This Document Requires Elsewhere

| Target | Amendment | Tracked by |
|---|---|---|
| DOC-45 §9.1 / §9.3 / §9.5, `C-1.3`, §14 Q-A/Q-B | The audit record shape, and the PROVISIONAL markers the 2026-08-03 answers discharge | **#4667** — authoritative; read it rather than a summary |
| DOC-57 §8.2 / PM-4 | Reconcile the tab table with the shipped six tabs, and record OQ-1's outcome | filed with OQ-1's answer |
| #4567 | Restate acceptance criteria against the amended record shape | **#4667** blocks it |

---

## 15. References

[DOC-45](./DOC-45-credential-authority-model.md) (the authority),
[ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md) (§11's
basis), [DOC-57](./agent-config-five-sections.md) (the configuration surface,
OQ-1), [DOC-63](./DOC-63-okg-sources.md) §7/§12 (the precedent for rendering
credential state without a credential); epic **#4040**'s owner comment of
2026-08-03 (the authoritative record of Q1–Q4); issues **#4566**, **#4567**,
**#4667**, **#4632**, **#4646**.

---

## 16. Revision History

| Date | Change |
|---|---|
| 2026-08-03 | Initial draft (#4663). Specifies the five panel acts, the never-display-a-value constraint, and the audited-event shapes. Found that DOC-45's landed audit record cannot carry those events; filed as **#4667**. |
| 2026-08-03 | Simplified on owner feedback (*"too complicated"*). Cut 53% (944 → 439 lines): extended rationale, restatements of decisions already recorded in DOC-45 / ADR-0026 / #4040, and the DOC-45 amendment analysis (now #4667's, cited in one clause). §§5, 6, 8 and 9.1 became clause tables; prose was kept only where the security reasoning is load-bearing. Reduced five open questions to the two that block building — PM-4 exemption (was OQ-3) and store-vs-grant-set (was OQ-4); the other three became stated assumptions in §3 and §7. Section numbers, spec IDs, and clause ids are unchanged where the clause survived; `P-8.12`–`P-8.15` were removed with their content living in #4667, and `P-8.11` / `P-8.16` retained. |
