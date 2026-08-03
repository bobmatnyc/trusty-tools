---
spec_refs:
  - id: SPEC-CREDAUTH-07~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-07~draft
  - id: SPEC-CREDAUTH-01~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-01~draft
  - id: SPEC-CREDAUTH-04~draft
    path: docs/specs/DOC-45-credential-authority-model.md
    anchor: SPEC-CREDAUTH-04~draft
  - id: SPEC-AGENTCFG-06~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-06~draft
  - id: SPEC-AGENTCFG-07~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-07~draft
  - id: SPEC-OKGSRC-10~draft
    path: docs/specs/DOC-63-okg-sources.md
    anchor: SPEC-OKGSRC-10~draft
---

# DOC-64 — The Credentials Panel: Per-Assistant Credential Sets, Transfer, and the User-Granted Copy

**Status:** Draft
**Spec ID:** `SPEC-CREDPANEL-01~draft` … `SPEC-CREDPANEL-09~draft` (DOC-64)
**Subsystem:** `trusty-agents` — the assistant configuration surface (panel, backing route, audited actions); `trusty-common` — the authority the panel is a client of (`Principal`, `CredentialRef`, grants, revocation state, the audit stream). The panel owns **no** authority of its own.
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-08-03
**DOC-N claim:** `DOC-64`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free four ways, because the tree scan alone is documented as insufficient (`scripts/check_doc_numbers.sh` says so in its own header, and two spec passes collided on `DOC-62` on 2026-08-01 for exactly that reason): (1) no file under `docs/specs/**` or `docs/trusty-installer/research/02-design/**` claims `DOC-64` by filename or header self-label on `origin/main` (`99f085a3`) — the highest claimed number there is `DOC-63`; (2) no OPEN pull request claims it — the four open PRs are #4526, #4578, #4640 and #4641, none a spec; (3) `scripts/check_doc_numbers.sh` reports `95 doc(s) / 89 claim(s) … 0 violations` and its NEXT-FREE check accepts the catalog hint, which reads `DOC-64`; (4) the only remote branch carrying an unmerged numbered spec is `spec-twin-lead-architecture`, which claims `DOC-44` alone. `DOC-63` was the previous high-water mark, so this is an advance of it rather than a back-fill.
**Builds on:** [DOC-45](./DOC-45-credential-authority-model.md) `SPEC-CREDAUTH-01~draft` (principal identity), `-03~draft` (grants and default-deny), `-04~draft` (the delegation boundary), `-06~draft` (revocation as an observable signal), `-07~draft` (the audit trail — **this document requires an amendment to it, §10**); [DOC-57](./agent-config-five-sections.md) `SPEC-AGENTCFG-06~draft` (Permissions) and `SPEC-AGENTCFG-07~draft` (the GUI section mapping this panel would extend); [DOC-63](./DOC-63-okg-sources.md) `SPEC-OKGSRC-10~draft` (the precedent for a per-assistant configuration sub-surface that renders credential state without rendering a credential)
**Decision record:** [ADR-0026 — A credential grant does not survive delegation](../adr/0026-credential-grants-do-not-survive-delegation.md)
**Related issues:** **#4663** (this spec); **#4040** (parent epic — the owner answers of 2026-08-03 that this document encodes); #4565 (`CredentialRef` / `Secret` — **landed**, `0c0de180`), #4566 (principal identity + ACL — the `Principal::Assistant` variant this panel is addressed to), #4567 (the audit stream every panel action writes to), #4570 (at-rest storage), #4632 (`SecretString`'s four-character disclosure — an **active** hazard), #4646 (unsolicited assistant-to-owner notifications — the delivery path for a copy request, §13 OQ-2)

---

## 1. Executive Summary

The owner's 2026-08-03 answers on epic #4040 made segmentation the goal —
**one credential store per assistant instance** — and, in the same breath,
created a thing that did not previously need to exist: a place where the user
performs the acts segmentation implies. An assistant may **ask** for a
credential another assistant holds; **only the user grants**. Without a surface
for that grant to happen in, "only the user grants" has nowhere to be enforced
and the rule is advisory.

This document specifies that surface. It is a **client of the authority**
DOC-45 ships, never a second authority: every action here resolves to a grant
issued or revoked by the Operator principal (DOC-45 `C-3.6`), and every action
emits a record on the one audit stream (#4567).

**The panel never displays a secret value.** It operates on `CredentialRef`s and
metadata. §4 makes that a construction property rather than a discipline, and
names the one type in this workspace that already violates it.

### 1.1 What is settled here, in one table

| # | Question | Settled by |
|---|---|---|
| 1 | What the panel is, and what authority it holds | §3 — a client of `trusty_common::credentials`; it holds none |
| 2 | What may cross the surface | §4 — refs and metadata only; **never** a value, never a prefix of one |
| 3 | Viewing a credential set | §5 — per assistant **instance**, with state, provenance and scope |
| 4 | Moving a credential between assistants | §6 — revoke-then-issue, atomic, never a copy |
| 5 | The copy request | §7 — an assistant **asks**, the user **grants**; never settled between assistants |
| 6 | Provenance and last-use | §8 — derived from the audit stream, never from a second ledger |
| 7 | Revocation, and what it means in flight | §9 — the next resolution fails; an in-flight call is not retracted, and this is stated rather than implied |
| 8 | The event shape of every panel action | §10 — and **the amendment to DOC-45 §9.1 that carrying them requires** |
| 9 | What a sub-agent gets from any of this | §11 — **nothing**, per ADR-0026 |

### 1.2 What is NOT settled here

- **Where the panel physically lives** — the assistants GUI, a `tm`/`tagent` CLI
  surface, or both. §13 OQ-1 states what exists today and what each choice costs.
- **How a copy request reaches a user who is not present.** §13 OQ-2; it
  intersects epic #4646.
- **Whether a "credential set" is a store or a grant set.** §13 OQ-4. The Q3
  answer says *store*; DOC-45 §5 models *grants* against a shared ref. The two
  readings produce different semantics for §6's move, and the difference is not
  cosmetic.
- **The three Q1 sub-questions the owner explicitly deferred** — namespace
  scope, the headless/SSH answer, and encryption of the file store. §12 states
  where this design would change if each resolves differently. This document
  assumes none of them.
- **Any capability grant.** This document adds nothing to
  `ASSISTANT_REACHABLE_SUBAGENTS`, widens no persona's reach, and grants no
  credential to anything. It specifies a surface for the Operator to exercise
  authority they already hold.

### 1.3 Verified current state

Measured on `origin/main` (`99f085a3`). Every row was read for this document.

| Claim | Verified |
|---|---|
| `CredentialRef` and `Secret<T>` exist and are clean | `crates/trusty-common/src/credentials/handle.rs:114` and `secret.rs:72`, landed by #4565 as commit `0c0de180`. `Secret<T>`'s `Debug` (`secret.rs:106`) and `Display` (`secret.rs:118`) are `impl<T>` — **no `T: Debug` bound** — so the formatter structurally cannot read the value. Pinned by `debug_and_display_are_value_independent`. |
| They have **zero** consumers outside `trusty-common` | No crate outside `trusty-common` constructs, stores, or renders a `CredentialRef`. A credentials panel would be the first. |
| There is **no** `Principal::Assistant` variant | `crates/trusty-common/src/credentials/principal.rs` — `Principal` is `#[non_exhaustive]` with `Operator` and `Service` only. `Assistant` and `SubAgent` land with #4566. The panel's whole subject does not exist yet as a type. |
| `resolve` checks no grant | `crates/trusty-common/src/credentials/mod.rs:36` — *"This module holds **no** authorization."* Authorization is #4566. |
| The assistant configuration surface is a six-tab full-pane takeover | `crates/trusty-agents/ui/src/components/AgentConfigPanel.svelte:110` — `personality`, `knowledge`, `skills`, `subagents`, `listeners`, `permissions`. DOC-57 §8.2 specifies five; `subagents` was added by #4029. **There is no credentials tab.** |
| That surface has never had a credential write path | `AgentConfigPermissions.svelte:14` restates DOC-57 **PM-4**: Permissions is read-only in every phase, because *"granting capability from a GUI is a security-relevant write path"*. A credentials panel is exactly such a path. §13 OQ-3. |
| What the surface renders about credentials today is a boolean | `crates/trusty-agents/ui/src/lib/agentConfig.ts:394` — `AgentSkillProvider { provider, requirement, env_var, configured }`, rendered as a chip. The backing check is `std::env::var(var).map(\|v\| !v.trim().is_empty())` (`crates/trusty-agents/src/api/server/agent_skills.rs:315`) — process-global, not per-assistant. |
| The only credential CLI is provider-global | `crates/trusty-common/src/inference/config/keys.rs:48` — `set` / `list` / `test` / `unset`, keyed on a provider slug with **no agent dimension**, mounted as `tm config keys` and `tagent config keys`. It deliberately has no `get` (`keys.rs:38`), pinned by `config_keys_rejects_get`. |
| An assistant's MCP secrets are plaintext in a hand-editable file | `crates/trusty-agents/src/mcp/config/types.rs:181` — `McpService.env: HashMap<String, String>`, serialised into `~/.trusty-agents/config.toml`. |
| `SecretString` discloses four characters of the live value | `crates/trusty-common/src/inference/types/secret.rs:80` renders `SecretString({redact_secret(..)})`, and `crates/trusty-common/src/credentials/redact.rs:43` returns `<first 4 chars>…(N chars)`. Pinned by `redact_secret_masks_tail` and `secret_debug_is_redacted`. #4632, **open**. |

---

## 2. Scope, Non-Goals, Terms

### 2.1 Scope

The surface: what a user sees of an assistant instance's credential set, the
five acts they may perform on it (view, move, approve/deny a copy, inspect
provenance and last-use, revoke), the event each act emits, and the constraint
that no act may render a value.

### 2.2 Non-goals

- **The authority.** DOC-45 owns principal identity, grant expression,
  default-deny, resolution, and revocation state. This document specifies a
  client of it and re-specifies none of it.
- **The store.** #4570 owns at-rest storage. §12 states where this design turns
  on its deferred sub-questions; it settles none of them.
- **Credential acquisition.** How a credential first enters a store — OAuth
  consent, an API key paste, a device flow — is out of scope. This document
  starts from a credential the authority already holds.
- **Any widening of reach.** Stated twice on purpose (§1.2).
- **The notification transport.** #4646 owns it. §13 OQ-2 states the seam.

### 2.3 Terms

| Term | Meaning in this document |
|---|---|
| **Panel** | The credentials surface this document specifies. |
| **Credential set** | What one assistant instance's principal can resolve. §13 OQ-4 records that "set" is ambiguous between *store contents* and *grant set*, and why that matters. |
| **Instance** | An `AssistantInstanceId` (`crates/trusty-agents/src/assistants/instance.rs:55`). `izzie` and `cto-assistant` are two instances, per the Q3 answer. |
| **Move** | Revoke from A, issue to B. §6. |
| **Copy request** | An assistant's *ask* for a credential another assistant holds. Never a grant. §7. |
| **Panel action** | Any of the five acts. Every one is an audited event (§10). |

### 2.4 Normative language

**MUST**, **MUST NOT**, **SHALL**, **SHALL NOT**, and **MAY** carry their
ordinary RFC-2119 force. Clauses are numbered `P-<section>.<n>` and are the
citable unit. A clause marked **BLOCKED** names the open question or unlanded
dependency it waits on and **MUST NOT** be implemented as written until that
clears.

---

## 3. SPEC-CREDPANEL-01 — What the Panel Is, and What Authority It Holds {#SPEC-CREDPANEL-01~draft}

**ID:** `SPEC-CREDPANEL-01~draft`
**Status:** Draft.

### 3.1 A client, not an authority

**P-1.1** The panel SHALL hold **no** authorization logic. Every act it offers
SHALL resolve to a call against `trusty_common::credentials` that the authority
evaluates, records, and MAY refuse. A panel that decided anything itself would
be DOC-45 `C-3.3`'s forbidden second entry point wearing a GUI.

**P-1.2** The panel SHALL act **as the Operator principal**, and only as the
Operator principal. DOC-45 `C-3.6` reserves grant issue and revoke to the
Operator; the panel is the surface through which that reservation is exercised,
not an exemption from it.

**P-1.3** The panel SHALL NOT be reachable by an assistant as a tool. An
assistant's only means of affecting a credential set is the *request* of §7,
which is a message to the user and confers nothing. A tool that let an assistant
drive the panel would restore the assistant-to-assistant copy path the owner
rejected, one indirection out.

### 3.2 The unit the panel is addressed to

**P-1.4 — BLOCKED on #4566.** The panel's subject is one
`Principal::Assistant(AssistantInstanceId)` per assistant instance, per the
owner's Q3 answer (*"ONE CREDENTIAL STORE PER ASSISTANT INSTANCE… the stated
goal is SEGMENTATION"*). That variant does not exist:
`crates/trusty-common/src/credentials/principal.rs` carries `Operator` and
`Service` only. Nothing in this document is implementable before #4566 adds it.

**P-1.5** Two instances of the same persona type SHALL be separately displayed,
separately granted, and separately revocable. `izzie` and `cto-assistant`
resolve against separate sets even though both derive from the `assistant`
persona. This is DOC-45 `C-1.3` with its PROVISIONAL marker discharged by the
Q3 answer — see §14 for the amendment that discharge implies.

**P-1.6** Segmentation SHALL have no escape hatch. Every path by which a
credential reaches an instance that did not hold it — the move of §6 and the
approved copy of §7 — SHALL route through the Operator and SHALL be recorded.
A path that did neither would defeat segmentation via its own remedy, which is
the reasoning the owner gave for the Q3 copy-path decision and which
[ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md) records
as the virtual-twin principle: assistants communicate; **authority is not
transferable**.

---

## 4. SPEC-CREDPANEL-02 — The Panel Never Displays a Value {#SPEC-CREDPANEL-02~draft}

**ID:** `SPEC-CREDPANEL-02~draft`
**Status:** Draft. This is the document's hardest constraint and the one with a
live counter-example in the tree.

### 4.1 The prohibition

**P-2.1** No surface of the panel — a list row, a detail view, a tooltip, a
copy-to-clipboard action, an error message, a CLI table, a JSON response body,
a log line emitted while rendering, or an audit record written by a panel
action — SHALL contain a credential value, a substring of one, a prefix of one,
or a "for identification" fragment of one.

**P-2.2** The panel SHALL identify a credential by its `CredentialRef` and its
metadata alone. `CredentialRef` is non-secret by construction (DOC-45 `C-2.1`)
and its restrictive grammar (`C-2.4`) is what makes rendering it verbatim safe —
`crates/trusty-common/src/credentials/handle.rs` enforces lowercase-kebab
segments, at most one `/`, at most 64 bytes, and routes `Deserialize` through
`parse` so an arbitrary string cannot enter the type. `C-2.6` requires `Display`
to render it verbatim, and the panel does.

**P-2.3** There SHALL be no "reveal" affordance, no masked-input round trip that
returns the stored value, and no read verb on any panel route that returns one.
This matches the deliberate absence of a `get` verb from the existing key CLI
(`crates/trusty-common/src/inference/config/keys.rs:38`, pinned by
`config_keys_rejects_get`) — the panel widens the surface, it does not relax the
rule.

### 4.2 Why this is achievable — the property the panel depends on

**P-2.4** `Secret<T>` is the type that makes `P-2.1` a construction property
rather than a discipline. Its `Debug` and `Display` impls
(`crates/trusty-common/src/credentials/secret.rs:106` and `:118`) are declared
`impl<T>` — **they carry no `T: Debug` bound.** The formatter therefore cannot
consult the wrapped value even if a future edit wanted it to; both write the
constant `Secret(<redacted>)` and read nothing. That is materially stronger than
an impl that chooses not to render, and it is the property this document relies
on. It is pinned by `debug_and_display_are_value_independent`, and reinforced by
the coherence guard in `secret.rs:149`'s `not_serialize_not_clone` module, which
fails the build with E0119 if `Serialize` or `Clone` is ever derived on
`Secret<T>`.

**P-2.5** No panel type SHALL hold a `Secret<T>`, or any other value-bearing
wrapper. The panel operates one level above resolution: it never calls
`resolve`, so it never has a value to leak. Where the type system permits, a
panel response type MUST NOT be constructible from a resolved credential.

### 4.3 The hazard the panel must not reproduce — #4632

**P-2.6** The panel SHALL NOT render, transport, or store `SecretString`, and
SHALL NOT use `redact_secret`.

`SecretString`'s `Debug` (`crates/trusty-common/src/inference/types/secret.rs:80`)
renders `SecretString({redact_secret(&self.0)})`, and `redact_secret`
(`crates/trusty-common/src/credentials/redact.rs:43`) returns the **first four
characters of the live value** plus its exact byte length. For any real API key
that is `sk-o…(51 chars)` — a four-character disclosure and a length oracle.
This is #4632, it is **open**, and it is pinned by three tests
(`redact_secret_masks_tail`, `redact_secret_short_inputs_table`,
`secret_debug_is_redacted`), which is why it cannot be quietly corrected.

**P-2.7** The reason this clause is stated as a prohibition rather than a
preference: a four-character head is exactly the shape a designer reaches for
when a list of credentials needs to be visually distinguishable, and the tree
already contains a function that produces it. The distinguisher is the
`CredentialRef` (`P-2.2`), which is designed to be readable, and nothing else.

**P-2.8** A test SHALL assert that no byte of a stored credential appears in any
panel response, rendered view, or panel-emitted audit record — the same shape as
DOC-45 `C-7.6`, applied to this surface.

---

## 5. SPEC-CREDPANEL-03 — View an Assistant Instance's Credential Set {#SPEC-CREDPANEL-03~draft}

**ID:** `SPEC-CREDPANEL-03~draft`
**Status:** Draft.

### 5.1 What a row carries

**P-3.1** The panel SHALL list, per assistant instance, one row per
`CredentialRef` in that instance's set. A row SHALL carry:

| Field | Source | Notes |
|---|---|---|
| `credential_ref` | the grant | Rendered verbatim (`P-2.2`) |
| `provider` | the provider registry (#4564) | The registry entry the ref resolves through, DOC-45 `C-2.7` |
| `scope` | the grant | Read/write plus provider-native scopes, DOC-45 `C-3.8` |
| `state` | the authority | One of `Live` / `Expired` / `Revoked` / `NeedsReauth`, DOC-45 `C-6.1` |
| `expiry` | the grant | The grant's expiry instant where it has one; explicitly `none` where it does not |
| `provenance` | §8 | How this instance came to hold it |
| `last_use` | §8 | Derived from the audit stream |
| `shared_with` | the authority | The other instances holding a grant against the same ref — see `P-3.4` |

**P-3.2** `state` SHALL be **queried from the authority**, never inferred. DOC-45
`C-6.2` requires a consumer to be able to query state without attempting a
resolution, and the panel is the archetypal such consumer: rendering a list of
twelve credentials must not resolve twelve secrets. A panel that resolved in
order to display would create the exact exposure `P-2.5` exists to prevent.

**P-3.3 — Never fabricate.** Loading, empty, and error SHALL be three distinct
rendered states. An instance with no credentials SHALL render an explicit empty
state, not a zero-length list styled as success; an authority that is
unreachable SHALL render an error naming the reason, not an empty set. This is
DOC-57 **G-4** and DOC-63 **S-10.7** applied unchanged, and it matters more here
than on any other pane: an empty credential list that is really a failed query
reads as *"this assistant holds nothing"*, which is the single most misleading
thing this surface can say.

**P-3.4** Where the same `CredentialRef` is held by more than one instance, each
row SHALL disclose that. Segmentation is a property the user is entitled to
audit, and a surface that showed each instance in isolation would let a
credential fan out across every assistant without that ever being visible in one
place.

### 5.2 What a row must not carry

**P-3.5** A row SHALL NOT carry a value, a value fragment, a length, a hash of
the value, or any field derived from the value (`P-2.1`). A length and a hash
are both oracles: a length distinguishes credential kinds and, with `#4632`'s
head preview, narrows a value materially; a hash confirms a guess.

**P-3.6** A row SHALL NOT carry a `Missing` vs `Denied` distinction in any form
an assistant can read. DOC-45 `C-5.10` forbids that distinction becoming an
enumeration oracle in model-visible context. The panel is **operator-visible and
not model-visible**, so it MAY render the full distinction — but only if the
panel's rendering is genuinely not reachable by an assistant, which `P-1.3`
requires and which an implementation MUST pin rather than assume.

---

## 6. SPEC-CREDPANEL-04 — Move a Credential Between Assistants {#SPEC-CREDPANEL-04~draft}

**ID:** `SPEC-CREDPANEL-04~draft`
**Status:** Draft. Semantics depend on §13 OQ-4.

### 6.1 What a move is

**P-4.1** A move SHALL be **revoke-from-source then issue-to-target**, and SHALL
NOT be a copy. On success the source instance no longer resolves the ref and the
target does. A "move" that left the source holding the credential would be a
copy with a misleading label, and the copy path has its own gate (§7) precisely
because it is a different act with a different risk.

**P-4.2** A move SHALL be **atomic from the user's point of view**: either both
halves take effect or neither does. A partial move that revoked the source and
failed to issue to the target would silently break the source assistant, and a
partial move that issued to the target and failed to revoke the source would
silently produce the copy `P-4.1` forbids — the second failure being the
dangerous one, because it looks like success.

**P-4.3** A move SHALL carry the source grant's scope to the target **unchanged
or narrowed**, never widened. Where the user wants a wider scope on the target,
that is a separate issue-grant act with its own record. A move that could widen
would make the move the cheapest way to escalate.

**P-4.4** A move SHALL be refused, with a stated reason, when the source holds no
grant for the ref, when the target already holds one, or when the source grant is
in state `Revoked`. Each refusal SHALL be a recoverable error carrying an
actionable message (DOC-45 `C-3.11`, `C-5.7`), never a silent no-op.

### 6.2 What a move does not do

**P-4.5** A move SHALL NOT move the secret's bytes anywhere. Under the
grant-set reading (§13 OQ-4) it rewrites two grant rows and touches no stored
value at all; under the store reading it is a store-to-store transfer performed
entirely inside the authority, and the value never enters the panel's process
boundary in either case (`P-2.5`).

**P-4.6** A move SHALL NOT affect any sub-agent. Per ADR-0026 and DOC-45 `C-4.2`,
a `SubAgent { name, delegator }` is a distinct principal; moving a credential
between two `Assistant` principals grants nothing to either one's sub-agents and
revokes nothing from them. §11 states this in full because it is the property
most likely to be assumed away.

---

## 7. SPEC-CREDPANEL-05 — The Copy Request: An Assistant Asks, Only the User Grants {#SPEC-CREDPANEL-05~draft}

**ID:** `SPEC-CREDPANEL-05~draft`
**Status:** Draft. **Owner decision, 2026-08-03 — settled, not open.**

> An assistant may ASK for a credential held by another assistant; ONLY THE USER
> GRANTS. The copy is an auditable act in the credentials panel, never settled
> between two assistants.
>
> — Owner, on #4040, 2026-08-03

The reasoning the owner gave, carried here because it is the clause's whole
justification: an assistant-to-assistant copy that does not route through the
user **defeats segmentation via its own escape hatch**, and contradicts both
ADR-0026 and the virtual-twin principle that assistants communicate but
authority is not transferable.

### 7.1 The request

**P-5.1** A request SHALL be a durable, addressable object carrying: a request
id, the **requesting** principal, the **holding** principal, the
`CredentialRef`, the requested scope, a free-text reason supplied by the
requester, and a creation instant.

**P-5.2** The reason field SHALL be treated as **untrusted content**. It is
authored by a model, it is displayed to a user who is about to make a security
decision, and it is therefore the highest-value prompt-injection target on this
surface. It SHALL be rendered as inert text — never as markup, never as a link,
never interpolated into any instruction context — and it SHALL be length-bounded.
DOC-63 §6 establishes this repo's active threat model on exactly this shape.

**P-5.3** A request SHALL confer **nothing** while it is open. The requesting
principal's effective reach is unchanged from creation until the user grants,
and an expired or denied request leaves it unchanged permanently. Creating a
request SHALL NOT be usable as a signal by any other code path.

**P-5.4** A request SHALL be rate-bounded per `(requesting principal, ref)`. An
assistant that can create requests without bound can turn the approval surface
into a nuisance channel until the user approves to make it stop, which is
consent by attrition and is not consent.

### 7.2 The grant

**P-5.5** Only the Operator SHALL resolve a request, and only through the panel
(`P-1.2`, `P-1.3`). No assistant, sub-agent, service, or automation SHALL be
able to approve one — including the **holding** assistant. The holder's consent
is not a factor: the holder never had transferable authority to consent with.

**P-5.6** Approval SHALL create a **new, independent grant** to the requesting
principal. It SHALL NOT alias, share, or reference the holder's grant, and
revoking either grant afterwards SHALL NOT affect the other. Two independent
grants against one ref is what DOC-45 `C-2.3` already permits, and it is what
keeps each instance separately revocable (`P-1.5`).

**P-5.7** The approving user SHALL be able to grant a **narrower** scope than the
one requested, and SHALL NOT be able to grant a wider one from this surface.
Widening is an issue-grant act, not an approval.

**P-5.8** Approval SHALL be per-request and SHALL NOT be standing. There is no
"always allow this assistant to copy from that one" — a standing approval is the
assistant-to-assistant channel the owner rejected, expressed as a setting.

**P-5.9** Denial SHALL be a first-class outcome with its own record (§10), not
the absence of an approval. A request that reaches its expiry without a decision
SHALL resolve to `timed_out`, which is a third outcome and SHALL NOT be recorded
as a denial — the two are different facts about what the user did, and DOC-45
`C-10.8` already requires exactly this three-valued shape for its confirmation
gate.

**P-5.10** The panel SHALL display, at decision time: both principals, the ref,
the requested scope, the requester's reason (per `P-5.2`), the holder's current
scope, and **what the requester's set would look like afterwards**. A user
approving a copy is deciding about a resulting reach, not about a row.

---

## 8. SPEC-CREDPANEL-06 — Provenance and Last-Use {#SPEC-CREDPANEL-06~draft}

**ID:** `SPEC-CREDPANEL-06~draft`
**Status:** Draft. Depends on §10's amendment.

### 8.1 Provenance

**P-6.1** Each row SHALL display **how this instance came to hold this
credential**, as one of: `issued_directly` (the Operator issued it), `moved_from
<principal>` (§6), `copy_approved_from <principal>` (§7), or `provisioned`
(established by a bundled or template configuration). Provenance SHALL name the
originating principal where one exists.

**P-6.2** Provenance SHALL be **derived from the audit stream**, not stored in a
second ledger. A parallel provenance record could disagree with the audit trail,
and where the two disagree the audit trail is what an incident review reads —
so the display would be the lie. This is the same reasoning DOC-63 `S-10.3`
applies to its run log: a display record is never an input to a correctness
decision.

**P-6.3** Where the audit stream has been rotated or truncated past the
originating event, provenance SHALL render `unknown`, explicitly. It SHALL NOT
render `issued_directly` as a default, which would assert a fact the system no
longer holds. DOC-45 `C-7.12`'s bounded local sink makes this a real case, not a
hypothetical.

### 8.2 Last-use

**P-6.4** Each row SHALL display the instant of the most recent **allowed**
resolution by this principal against this ref, and separately the most recent
**denied** attempt. One number that merged the two would hide the more
interesting event: an assistant repeatedly attempting a credential it is not
granted is a signal, and it is invisible if denials fold into "last used".

**P-6.5** Last-use SHALL be derived from the audit stream, per `P-6.2`. This is
the direct beneficiary of the owner's Q2 per-call answer: aggregation per session
or per grant cannot answer *"when was this last used"* at the grain a revocation
decision needs, and the owner's stated rationale for per-call — *"the only grain
that can surface a pattern such as one key being read hundreds of times in a
loop"* — is precisely a panel-facing rationale.

**P-6.6** A credential never resolved SHALL render an explicit `never used`, not
an empty cell and not the grant's creation time. `never used` is the most
actionable row on this surface: it is a grant the user can revoke at no cost.

**P-6.7** Last-use SHALL NOT be presented as complete while DOC-45 `C-3.12`
holds. 55 raw `std::env::var("<CREDENTIAL>")` reads across 36 files bypass the
resolver today, and every one is a use that leaves no record. The panel SHALL
state that its last-use figure covers resolutions through the authority, rather
than implying it covers all use. #4571 is what makes the caveat removable, and
the caveat SHALL NOT be removed before it lands.

---

## 9. SPEC-CREDPANEL-07 — Revocation, and What It Means In Flight {#SPEC-CREDPANEL-07~draft}

**ID:** `SPEC-CREDPANEL-07~draft`
**Status:** Draft.

### 9.1 The act

**P-7.1** Revoking from the panel SHALL revoke **one grant** — the
`(Principal, CredentialRef)` pair the row names — and nothing else. It SHALL NOT
delete the stored credential, SHALL NOT revoke other principals' grants against
the same ref, and SHALL NOT contact the provider. Per-row revocation is what
makes `P-1.5`'s segmentation operable: revoking `izzie`'s Gmail reach must not
touch `cto-assistant`'s.

**P-7.2** The panel SHALL distinguish, in its wording and in its confirmation,
between revoking a **grant** (local, authority-side, immediate, the act this
surface performs) and the credential being **revoked upstream** by the provider.
DOC-45 `C-6.4` names these as two different kinds of death; conflating them on
the one surface where a user acts on them would make the surface teach the wrong
model.

**P-7.3** Revocation SHALL take effect on the authority's state before the panel
reports success. A panel that reported success optimistically would tell a user
their exposure had closed when it had not, which is the worst failure this
surface has.

### 9.2 In-flight resolutions — stated, not implied

**P-7.4** Revocation SHALL cause **the next resolution to fail**. After the
revoke commits, any `resolve(ref, principal, scope)` for that pair returns
`Expired` or `Denied` (DOC-45 `C-5.2`, `C-5.3`) **before any network call is
attempted** (`C-6.3`).

**P-7.5** Revocation SHALL NOT retract a `Secret` already resolved and in use.
There is no mechanism by which the authority can reach into a call already
issued, a subprocess already spawned with the value in its environment
(DOC-45 `C-8.4`), or an HTTP request already on the wire. An in-flight operation
completes.

**P-7.6** The window SHALL be **bounded by resolve-at-use-time**, and the panel
SHALL say so rather than implying instantaneous cutoff. DOC-45 `C-8.4` requires
resolution at use time and never at config-load or spawn-list-construction time,
so the exposure after a revoke is one operation long, not one session long. That
is the honest claim and the panel makes it: *revocation stops the next use; an
operation already running finishes.*

**P-7.7** Where the exposure matters more than the in-flight operation, the
remedy is **rotation at the provider**, which is outside this panel and which
the panel SHALL name as the next step in its revoke confirmation. Rotation does
not invalidate grants (DOC-45 `C-6.7`) — the ref is stable across it — so the
two acts compose: revoke the grant here, rotate upstream there.

**P-7.8** Revocation SHALL notify subscribers of the state transition (DOC-45
`C-6.2`), so a consumer such as a scheduled OKG source moves to its displayed
`needs-credential` state and stops (DOC-63 `S-5.4`) rather than failing silently
forever.

**P-7.9** Revocation SHALL be reversible only by **issuing a new grant**, which
is a separate act with its own record. There is no "undo" that restores the
revoked grant in place, because an undo would leave a revocation in the audit
trail that did not, in the end, revoke anything.

---

## 10. SPEC-CREDPANEL-08 — The Audited Events, and the DOC-45 Amendment They Require {#SPEC-CREDPANEL-08~draft}

**ID:** `SPEC-CREDPANEL-08~draft`
**Status:** Draft. **§10.3 is a finding, not a formality — it names a change to a
shipped spec.**

Every panel action is an audited event, per the owner's Q2 answer: **one stream
with a discriminator, records per resolution call, not aggregated.** This
section states the event shape for each of the five acts, and then states
plainly what DOC-45's already-landed record shape can and cannot carry.

### 10.1 One stream, and where panel events sit on it

**P-8.1** Panel events SHALL be emitted on the **same stream** as credential
resolution records, discriminated by a category field. That is the owner's Q2(ii)
answer applied within the credential domain as well as across the three domains
DOC-45 `C-7.9` enumerates. A separate panel-only stream would put *"the user
moved this credential"* and *"this credential was then resolved by the target"*
in two places that must be manually correlated, which is the exact failure a
single ordered timeline exists to prevent.

**P-8.2** Panel events SHALL obey DOC-45 `C-7.3`–`C-7.5` without exception: the
record type accepts a `CredentialRef`, never a `String` that might hold a value;
no record contains a credential, a partial credential, a request header, or a
URL query string; and this is not advisory. `P-5.2`'s untrusted reason field is
the one free-text member of any event below, and `P-8.9` bounds it.

**P-8.3** Panel events SHALL be independently suppressible from resolution
records (DOC-45 `C-7.11`), which under a single-stream topology means per-category
filtering rather than one on/off switch. Suppressing the high-volume resolution
records while retaining the low-volume administrative ones is the configuration
an operator will actually want, and a topology that cannot express it will be
worked around by suppressing everything.

### 10.2 The five event shapes

All five carry a common envelope — `timestamp`, `stream_category`,
`event_kind`, `actor` (always the Operator principal, `P-1.2`), `session_id`
(DOC-45 `C-1.5`), and `call_site` — plus the fields below.

**P-8.4 — `credential.view`.** Emitted when a credential set is rendered.
Carries: `subject` (the assistant instance principal whose set was viewed), and
`refs_listed` (the count, not the list). Reading a credential set is a
disclosure of an assistant's reach and is worth a record; the refs themselves are
already recoverable from the grant rows, so listing them again buys nothing and
grows the stream.

**P-8.5 — `credential.move`.** Carries: `from_principal`, `to_principal`,
`credential_ref`, `scope_before`, `scope_after`, and `outcome`
(`committed` | `refused`) with a `refusal_reason` where refused. A move is one
event with two principals, not two events — recording it as a revoke plus an
issue would lose the fact that they were one atomic act (`P-4.2`) and would make
`P-6.1`'s `moved_from` provenance unreconstructable.

**P-8.6 — `credential.copy_requested`.** Carries: `request_id`,
`requesting_principal`, `holding_principal`, `credential_ref`,
`requested_scope`, and `reason` (bounded, inert — `P-5.2`, `P-8.9`). The actor
on this one event is **not** the Operator: the requester is an assistant. The
envelope's `actor` field therefore carries the requesting principal, and this is
the single exception to `P-1.2`'s Operator-only rule — because the request itself
is the one act on this surface an assistant performs.

**P-8.7 — `credential.copy_decided`.** Carries: `request_id` (correlating to
`P-8.6`), `requesting_principal`, `holding_principal`, `credential_ref`,
`granted_scope` (present only on approval), and `outcome`, which is
**three-valued**: `approved` | `denied` | `timed_out` (`P-5.9`). The `request_id`
correlation is load-bearing: without it, an approval cannot be tied to the reason
that persuaded the user, and post-incident review of *"why did this assistant get
that key"* is unanswerable.

**P-8.8 — `credential.revoked`.** Carries: `subject` (the principal revoked
from), `credential_ref`, `revocation_kind` (`grant` — the panel performs only
this kind, `P-7.2`), `prior_state`, and `in_flight_note`: whether any resolution
by this principal against this ref occurred within the implementation's declared
in-flight window before the revoke committed. That last field is what makes
`P-7.5` reviewable rather than merely stated — it tells an incident reviewer
whether the revoke plausibly raced a use.

**P-8.9** `reason` (`P-8.6`) SHALL be the only free-text field on any event, SHALL
be length-bounded at the type level, and SHALL be stored and rendered as inert
text. DOC-45 `C-7.5` is explicit that the audit stream is the one place where a
bug converts a security control into a disclosure channel; a model-authored
string is the most likely carrier, and #4521 reaches the same conclusion
independently for shell command text.

**P-8.10** Every event SHALL be emitted **whether the act succeeded or was
refused**. A refused move and a denied copy are the more interesting forensic
events, exactly as a denial is on the resolution path (DOC-45 `C-7.2`), and they
are the ones an implementation optimising for the happy path drops.

### 10.3 The finding: DOC-45's landed record shape does not carry these

DOC-45 `C-7.1` is on `origin/main` and #4563 is **CLOSED**. Its record is:

> timestamp · principal · credential · requested scope · decision (allow/deny) ·
> denial variant · call site · session id · invocation id · project context

**P-8.11 — What fits.** The **per-call grain fits the landed shape unchanged.**
DOC-45 `C-7.7` already names *"one record per resolution call"* as its default,
and the owner's Q2(i) answer selects exactly that. No field changes for the
grain. What does change is `C-7.7`'s **status**: it is marked PROVISIONAL and
carries the instruction *"Do not implement aggregation, and do not implement a
retention policy that assumes aggregation, until Q-A is answered."* Q-A is
answered; that text is now stale and misdirects the next reader of #4567.

**P-8.12 — What does not fit, part one: the discriminator.** `C-7.1` has **no
category or discriminator field.** `C-7.9` posed the one-stream-versus-three
question and, being PROVISIONAL, added no field to resolve it either way. The
owner chose one stream with a discriminator. The landed record therefore cannot
express the topology that was chosen, and `C-7.1`'s field set must gain a field
it does not have.

**P-8.13 — What does not fit, part two: administrative events.** A panel action
is **not a resolution attempt**, and `C-7.2` — *"a record SHALL be emitted on
every resolution attempt"* — does not reach it. Four concrete mismatches, each
verified against the landed table:

1. **No event-kind field.** `C-7.1` cannot distinguish a move from a revoke from
   a copy approval. Every panel event would render as an untyped row.
2. **One principal, two needed.** `C-7.1` carries a single `principal`. A move
   (`P-8.5`) has a source and a target; a copy decision (`P-8.7`) has a requester
   and a holder. Neither is expressible.
3. **No correlation id.** There is no field binding `credential.copy_requested`
   to the later `credential.copy_decided`. `invocation id` is scoped to
   `SubAgent` principals (`C-1.6`) and is not a request correlator.
4. **`decision` is two-valued and the wrong axis.** `C-7.1`'s `decision` is
   *allow or deny*. A copy decision is three-valued (`P-5.9`); a move's outcome
   is `committed` or `refused`; a revoke has no allow/deny axis at all.
   `requested scope` and `denial variant` are meaningless on a revoke.

**P-8.14 — This under-specification predates the panel.** DOC-45 `C-10.8`
already requires that *"a confirmation SHALL be recorded in the audit stream with
its outcome (confirmed / declined / timed out)"* — a **three-valued** outcome
that `C-7.1`'s two-valued `decision` cannot hold. So the landed record already
carries one event it cannot represent, before this document adds any. This is
stated so the amendment is not read as a cost this panel imposes.

**P-8.15 — The required amendment, stated plainly.** DOC-45 §9.1 **must be
amended**, and the amendment is not optional if the panel's events are to sit on
the stream the owner specified. The amendment is:

- **(a)** add a stream-category discriminator to `C-7.1` and flip `C-7.9` from
  PROVISIONAL to settled at *one stream with a discriminator*;
- **(b)** flip `C-7.7` from PROVISIONAL to settled at *per resolution call*, and
  strike its hold on implementation;
- **(c)** generalise `C-7.1` from a resolution-only record into an envelope plus a
  per-kind payload, with an **event-kind** discriminator, so that a resolution
  record, an administrative record (§10.2), and `C-10.8`'s confirmation record are
  three kinds on one stream rather than one shape stretched over three uses;
- **(d)** add, for administrative kinds, the fields §10.2 requires and `C-7.1`
  lacks: a second principal, a request correlation id, and a per-kind outcome.

**P-8.16** That amendment SHALL be a **change to DOC-45**, filed as its own
issue, and SHALL NOT be performed by this document. #4563 is closed and DOC-45
shipped; a second document that quietly assumed a different record shape would
leave two specs disagreeing about the one artifact an incident review reads.
Until the amendment lands, `P-8.4`–`P-8.10` are **BLOCKED**: they are specified
here so the amendment has a concrete requirement to satisfy, not so they can be
built against a shape that does not exist. Whoever picks up #4567 must read
DOC-45 §9.1 first — the owner's own Q2 answer flags exactly this.

---

## 11. SPEC-CREDPANEL-09 — What a Sub-Agent Gets From Any of This: Nothing {#SPEC-CREDPANEL-09~draft}

**ID:** `SPEC-CREDPANEL-09~draft`
**Status:** Draft. **Owner decision, 2026-08-01, recorded as
[ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md).**

**P-9.1** No act on this panel SHALL confer anything on a sub-agent. Issuing,
moving, or approving a copy to `Assistant(izzie)` grants nothing to
`SubAgent { name: "version-control", delegator: izzie }`, which is a distinct
principal (DOC-45 `C-4.2`) resolving against its own grant, fail-closed
(`C-4.3`).

**P-9.2** The panel SHALL make this visible rather than leaving it to be
discovered. An instance's credential row SHALL NOT imply that the assistant's
sub-agents share the credential, and where the panel displays an assistant's
sub-agents at all (the `subagents` tab exists today —
`AgentConfigPanel.svelte:110`), a sub-agent SHALL be shown with its **own**
credential set or an explicit empty one.

**P-9.3** A sub-agent SHALL NOT be able to file a copy request against its
delegator's set. `P-5.1`'s requesting principal is an `Assistant`; permitting a
`SubAgent` to request from its own delegator would reconstruct inheritance as a
one-click approval, and inherit-narrowed is precisely what ADR-0026 rejected.

**P-9.4** The configuration cost is the point. ADR-0026 and DOC-45 `C-4.6` state
that this design **increases** configuration burden deliberately: each grant
becomes an explicit, listed, individually revocable row rather than an implicit
consequence of who called whom. This panel is where that burden is paid, and
`P-6.6`'s `never used` row is how it is kept from accumulating.

---

## 12. Where the Deferred Q1 Sub-Questions Would Change This Design

The owner answered #4040 Q1 as *"BOTH postures are acceptable for now"* —
keyring default, `0600` plaintext fallback, log loudly — and **explicitly
deferred** three sub-questions. This document assumes none of their answers.
Where each would land:

| Deferred sub-question | Where this design turns on it |
|---|---|
| **1. All credentials, or only a privileged `user.*` namespace** (#3076) | §5's row would gain a namespace column and §6's move would need a rule for whether a `user.*` credential is movable to an assistant at all. If a privileged namespace exists, the honest answer is likely that it is **not** movable and **not** copyable — but that is an owner call, not this document's. `P-4.1`/`P-5.6` are written namespace-blind and would need a carve-out. |
| **2. The headless/SSH answer** — hard-fail, escape hatch, or a different backend | The panel's `state` column (`P-3.1`) would gain an at-rest-posture indicator, because DOC-45 `C-9.7` requires diagnostics to say plainly that the posture on a headless host is `0600` plaintext. A hard-fail answer would additionally make some rows **unreachable** rather than merely differently stored, which is a distinct rendered state `P-3.3` would have to carry. |
| **3. Encrypting the file store** (#4034 Slice 7) | No change to any clause here. The panel never touches storage (`P-4.5`), so encryption of the fallback tier is invisible to it. Recorded as a non-impact so the question is not re-opened against this document. |

**P-10.1** No clause in this document SHALL be read as answering any of the
three. Where an implementation needs an answer it MUST escalate rather than
choose one.

---

## 13. Open Questions for the Owner

### OQ-1 — Where does the panel live: the assistants GUI, a CLI, or both?

**What exists today**, read from `origin/main` (`99f085a3`):

- **An assistants GUI exists.** Tauri 2 + Svelte, at
  `crates/trusty-agents/ui/`, product name *"Trusty Agents"*
  (`ui/src-tauri/tauri.conf.json:3`), talking to the `tagent` sidecar HTTP API
  (`crates/trusty-agents/src/api/server/routes.rs:144`). Its assistant
  configuration surface is a full-pane takeover
  (`ui/src/components/AgentConfigPanel.svelte`) with **six** tabs declared at
  `AgentConfigPanel.svelte:110`: `personality`, `knowledge`, `skills`,
  `subagents`, `listeners`, `permissions`. DOC-57 §8.2 specifies five; `subagents`
  was added by #4029. A credentials panel would be a **seventh** entry plus a new
  `AgentConfigCredentials.svelte` and a new backing route.
- **No CLI surface is per-assistant.** `tm agent` is `list` / `show` only
  (`crates/trusty-mpm/src/bin/tm/cli/mod.rs:332`,
  `cli/actions/agent.rs:24`). The only credential CLI is the shared, **provider-global**
  `config keys` — `set` / `list` / `test` / `unset`
  (`crates/trusty-common/src/inference/config/keys.rs:48`), mounted as
  `tm config keys` and `tagent config keys`. It is keyed on a provider slug with
  **no agent dimension at all**, so it cannot express "izzie's Gmail credential"
  today.

**The fork.** GUI-only is the smallest change and matches where every other
assistant configuration section lives. CLI-only or both would make the panel
usable over SSH — the deployment shape this repo is most often driven in, and the
one DOC-45 `C-9.7` singles out — but requires inventing the per-assistant
credential CLI that does not exist. A CLI approval prompt is also a materially
different consent surface from a GUI dialog, and `P-5.10`'s "what the requester's
set would look like afterwards" is harder to render honestly in a terminal.

**What this document assumes:** nothing. Every clause is surface-neutral.

### OQ-2 — How does a copy request reach a user who is not present?

`P-5.9` gives a request a `timed_out` outcome, which presumes the request had a
chance to be seen. Today it would have none: the assistants GUI shows a request
only while the user has the panel open, and `tagent`'s Telegram and Slack bots
are **inbound-reply-only** — there is no `notify_owner` tool anywhere in shipped
code, and the only unprompted egress is trusty-mpm's closed-vocabulary
Telegram/Slack alert loop (`crates/trusty-mpm/src/telegram/alerts.rs`), which an
assistant's own reasoning cannot push text through.

This intersects epic **#4646** (unsolicited assistant-to-owner notifications)
directly, and #4646's recorded owner decisions constrain the answer: **D3**
defines "unattended" as a last-human-turn timeout, and **D4** requires notify
**once**, then queue silently and surface everything on the owner's return — no
retry, no escalation to a second channel.

**The fork.** (i) Is a copy request one of the things #4646 may notify about, or
does it queue silently until the user next opens the panel? (ii) If it may
notify, does approving over a notification channel count as approving *in the
panel* for the purposes of the Q3 ruling — and if so, what is the consent surface
in a chat message, given `P-5.10`? (iii) What is the request expiry, and does
D4's "queue silently" mean a request outlives it?

**What this document assumes:** nothing. `P-5.9` names `timed_out` as an outcome
without specifying who or what set the clock.

### OQ-3 — Is this panel exempt from DOC-57 PM-4, and on what basis?

DOC-57 **PM-4** states: *"Permissions is read-only in every phase of this spec.
Granting capability from a GUI is a security-relevant write path; it requires its
own review and is out of scope."* Its conformance criterion **C-06.4** is *"no
route in this spec grants or widens a permission"*, and
`AgentConfigPermissions.svelte:14` restates it in the component itself.

This panel is exactly that write path. The owner's Q3 answer places the copy
approval *"in the credentials panel"*, which makes the panel a grant-issuing
surface by construction — it cannot be read-only and still discharge #4663.

**The fork.** Is this the "own review" PM-4 anticipated, discharged by this
document plus its PR review? Or does the owner want a separate authorization
step around the panel itself — a re-authentication, an operator confirmation
per write, or a `user_authority` check (DOC-41 §5.5 / #3074, which is still
unimplemented)?

**What this document assumes:** nothing. It specifies the acts and leaves the
gate around them open. Note that DOC-45 `C-10.6` already requires interactive
operator confirmation for *first use of a credential by a principal that has
never resolved it before*, which composes with — and does not answer — this
question.

### OQ-4 — Is a "credential set" a store, or a grant set?

The Q3 answer says **store**: *"ONE CREDENTIAL STORE PER ASSISTANT INSTANCE."*
DOC-45 §5 models **grants**: `C-2.3` says a `CredentialRef` names a credential
and not a grant, *"so a shared ref — the whole point of a store row that several
principals may be separately granted against — is expressible."* These are two
different data models and they produce two different meanings for §6's move:

- **Grant-set reading** — one stored credential, N grants. A move rewrites two
  grant rows and touches no stored bytes. `P-3.4`'s `shared_with` column is
  natural. Revoking one grant provably cannot affect another.
- **Store reading** — N stores, each with its own copy of the bytes. A move
  transfers bytes between stores. `shared_with` becomes a cross-store comparison
  the authority must compute. Rotation (DOC-45 `C-6.7`) has to reach every store
  holding that credential, or the ref stops being stable across rotation for all
  but one instance — which would break `C-2.2`.

**The fork.** Which is it? The store reading is what the Q3 wording says; the
grant-set reading is what DOC-45 as shipped describes, and it is the one that
keeps `C-2.2` and `C-6.7` true. They may also be reconcilable — one physical
store, per-instance grant partitions presented to the user *as* per-instance
stores — but that reconciliation is an owner decision about what the product
means, not an implementation detail.

**What this document assumes:** nothing. §6 is written so that `P-4.1`–`P-4.6`
hold under either reading, and `P-4.5` states the consequence for each.

### OQ-5 — Does the user see requests from all assistants in one place, or per assistant?

A per-assistant panel is the natural shape for §5's view. A pending copy request
has **two** assistants, and the requester is the one the user has no reason to
have open. A per-assistant-only surface would hide requests behind whichever
assistant the user last selected.

**The fork.** Is there a single cross-assistant pending-requests view alongside
the per-assistant sets, or does a request surface under both principals' panels?

**What this document assumes:** nothing. `P-5.10` states what must be displayed
at decision time; it does not state where.

---

## 14. Amendments This Document Requires Elsewhere

Named so they are filed rather than discovered.

| Target | Amendment | Why |
|---|---|---|
| **DOC-45 §9.1 / §9.3 / §9.5** | The four-part change of `P-8.15` | #4563 is closed and the record shape shipped. The owner's Q2 answer cannot be honoured by the landed `C-7.1`, and `C-10.8` already exceeds it (`P-8.14`). **This is the finding #4663 was asked to surface.** |
| **DOC-45 `C-1.3`** | Discharge the PROVISIONAL marker at **one namespace per instance** | The Q3 answer settles Q-B. `C-1.3` currently reads *"the owner has not ruled"*, which is no longer true, and `C-12.1` blocks #4566's `Principal::Assistant` variant on it. |
| **DOC-45 §14 Q-A / Q-B** | Mark both **answered**, dated 2026-08-03 | Two BLOCKING owner questions are closed; leaving them open misdirects #4566 and #4567. |
| **DOC-57 §8.2 / PM-4** | Reconcile the tab table with the shipped six tabs, and record the outcome of §13 OQ-3 | DOC-57 §8.2 specifies five tabs; `AgentConfigPanel.svelte:110` declares six (`subagents`, #4029). A credentials panel makes it seven and is a write path PM-4 forbids. |
| **#4567** | Restate acceptance criteria against the amended shape | Its bullet *"the record shape is documented in DOC-45"* is satisfiable only once DOC-45 carries a shape that fits both grains. |

---

## 15. References

- [DOC-45 — The Credential Authority Model](./DOC-45-credential-authority-model.md) — the authority this panel is a client of.
- [ADR-0026 — A credential grant does not survive delegation](../adr/0026-credential-grants-do-not-survive-delegation.md) — §11's basis.
- [DOC-57 — Five-Section Agent Configuration](./agent-config-five-sections.md) — the configuration surface (§13 OQ-1, OQ-3).
- [DOC-63 — OKG Sources](./DOC-63-okg-sources.md) §7, §12 — the consumer precedent for rendering credential state without a credential.
- [DOC-38 — Spec-Linked Documentation](./spec-linked-documentation.md) §4 — this document's own authoring rules.
- Epic **#4040**, owner answers of 2026-08-03 — the authoritative record for Q1 (both postures, three sub-questions deferred), Q2 (one stream, per call), Q3 (one store each; assistant asks, only the user grants), Q4 (ADR-0026).
- Issues **#4566**, **#4567**, **#4570**, **#4632**, **#4646**.
- Source: `crates/trusty-common/src/credentials/{handle,secret,principal,authority,error}.rs`; `crates/trusty-common/src/inference/types/secret.rs`; `crates/trusty-common/src/credentials/redact.rs`; `crates/trusty-agents/ui/src/components/AgentConfigPanel.svelte`; `crates/trusty-common/src/inference/config/keys.rs`.

---

## 16. Revision History

| Date | Change |
|---|---|
| 2026-08-03 | Initial draft (#4663). Encodes the owner's 2026-08-03 answers on #4040: one credential store per assistant instance (segmentation, §3), an assistant may ask but only the user grants (§7), one audit stream with a discriminator at per-resolution-call grain (§10), and both at-rest postures with three sub-questions deferred (§12). Specifies the five audited panel events and **finds that DOC-45's landed `C-7.1` record shape cannot carry them** — the per-call grain fits unchanged, the stream discriminator and every administrative event do not (§10.3, `P-8.11`–`P-8.16`). Records that `C-10.8`'s three-valued confirmation outcome already exceeds the landed shape, so the amendment is not a cost this panel introduces. Opens five questions: where the panel lives, how a request reaches an absent user (#4646), whether DOC-57 PM-4 exempts this write path, whether a credential set is a store or a grant set, and whether pending requests have a cross-assistant view. |
