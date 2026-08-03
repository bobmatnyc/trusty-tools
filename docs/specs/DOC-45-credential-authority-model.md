---
spec_refs:
  - id: SPEC-OKGSRC-05~draft
    path: docs/specs/DOC-63-okg-sources.md
    anchor: SPEC-OKGSRC-05~draft
  - id: SPEC-AGENTCFG-06~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-06~draft
---

# DOC-45 — The Credential Authority Model: Principals, Scoping, Revocation, Audit, and the Sub-Agent Boundary

**Status:** Draft
**Spec ID:** `SPEC-CREDAUTH-01~draft` … `SPEC-CREDAUTH-11~draft` (DOC-45)
**Subsystem:** `trusty-common` — the authority itself (principal, `CredentialRef`, ACL, revocation, audit, delivery); `trusty-agents` — assistant/sub-agent principals, MCP delivery, routed-shell environment scrubbing; `trusty-code` — service principals (the product with no assistant in the loop). Cross-product by owner decision: *"#4040 yes for agents and code."*
**Owner:** Engineering (trusty-common) / Bob Matsuoka
**Last-updated:** 2026-08-01
**DOC-N claim:** `DOC-45`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free four ways, because the tree scan alone is known to be insufficient (`scripts/check_doc_numbers.sh` says so in its own header, and two spec passes collided on `DOC-62` on 2026-08-01 for exactly that reason): (1) no file under `docs/specs/**` or `docs/trusty-installer/research/02-design/**` claims `DOC-45` by filename or header self-label on `origin/main`; (2) no OPEN pull request claims it — the only two open PRs are #4526 and #4578, neither a spec; (3) the `spec-twin-lead-architecture` branch claims **`DOC-44` only** (`docs/specs/DOC-44-engineering-lead-twin-orchestration.md`), and that branch's own catalog note reads *"Next free `DOC-N` = `DOC-45`"* — so it does not claim 45, and **this document's catalog change corrects the stale README note that said it did**; (4) the only prior claimant was PR **#3039** (*"docs(spec): DOC-45 — Remote MCP credential delivery for fleet sessions"*), which is **CLOSED**, and whose subject — issue #3038 — was folded into epic #4040 and is carried by this very document plus #4568. `DOC-45` is therefore not merely free, it is the number this work was reserved under.
**Related issues:** **#4563** (this spec + its ADR); **#4040** (parent epic: unified credential authority, delivery, and audit); #4564 (provider registry), #4565 (`CredentialRef`/`Secret`), #4566 (principal, ACL, delegation, revocation), #4567 (audit trail), #4568 (`McpService.env` → references, discharges #3038 Phase 1), #4569 (redaction consolidation), #4570 (at-rest storage — **unblocked by §11 below**), #4571 (consumer migration, discharges #4035–#4039); consumers **#4417**, **#4479**, **#4439** (Constraint 2 b–d only), **#4478** (question b), **#4550**, **#4531**/DOC-63
**Decision record:** [ADR-0026 — A credential grant does not survive delegation](../adr/0026-credential-grants-do-not-survive-delegation.md)

---

## 1. Executive Summary

Five issues are blocked on epic #4040 not for a library but for a **decision
record**. This document is that record's normative half: what a principal is,
what a grant scopes to, what a denied read returns, where a secret may and may
not live, what is audited, and — the question that blocked everything else —
**what a sub-agent inherits when an assistant delegates to it.**

The answer to that last question, given by the owner on 2026-08-01, is the spine
of this document: **a credential grant does not survive delegation.** A sub-agent
holds its own grant, checked against its own principal. An assistant cannot lend
reach it holds. §6 encodes it; [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md)
records why, and what was rejected.

**This document ships authority. It grants nothing.** No clause here widens what
any assistant, sub-agent, or service can reach. The capability grants that
*consume* this authority live in #4546–#4549 and #4417 / #4439 / #4479 and stay
blocked where they are.

### 1.1 What is settled here, in one table

| # | Question | Settled by |
|---|---|---|
| 1 | What can hold a credential grant | §3 — four principal kinds; **session collapses** into the assistant instance for authorization and survives only as an audit attribute |
| 2 | What a config row holds in plaintext | §4 — `CredentialRef`: opaque, durable, non-secret, stable across rotation |
| 3 | How a grant is expressed, and default-deny | §5 — `(Principal, CredentialRef, Scope)`; absent grant ⇒ `Denied`; a **code-owned floor** that config can only narrow |
| 4 | What a sub-agent inherits | §6 — **nothing.** Non-transitive, fail-closed, and secret-passing across a hop is forbidden |
| 5 | The four (five) diagnostics | §7 — `Missing` / `Denied` / `Expired` / `ZeroScope`, plus `ScopeUnavailable` as a distinct honest answer |
| 6 | Revocation as a signal, not an inference | §8 — authority-held state, checked before the network call, with a consumer report channel for upstream revocation |
| 7 | The audit record | §9 — field set, prohibitions, and unrepresentability-by-construction are settled; **cadence and stream topology are PROVISIONAL** pending owner Q-A |
| 8 | Delivery without leaking | §10 — subprocess env, HTTP header, remote session; and the assistant-home prohibition as a construction-time path check |
| 9 | Where secrets live at rest | §11 — keyring by default, `0600` file store as the fallback, **and the fallback logs loudly** |
| 10 | The sub-agent / project-trust boundary | §12 — project scope, cross-project audit, confirmation classes — the property #4417 / #4479 / #4439(2b–d) each asked for |
| 11 | The seam with #4550 | §13 — DOC-45 owns the *credential* dimension, #4550 owns the *command* dimension, they compose as AND, and the routed shell's environment is scrubbed |

### 1.2 What is NOT settled here

- **Owner Q-A — audit granularity and stream topology** (§14). Blocks #4567 and
  the PROVISIONAL clauses in §9.
- **Owner Q-B — credential namespace per assistant instance** (§14). Blocks
  #4566's principal granularity and the PROVISIONAL clause `C-1.3` in §3.
- **#4439 Constraint 1** — authority-holder gating of computer-use actions. That
  is the User-Authority Singleton model (DOC-41 §5.5, #3074), deliberately out of
  scope per #4040's own scheduling comment. §12.4 states the seam so the two
  compose rather than drift.
- **Encrypting the file store** (#4034 Slice 7). §11 settles *which* backend and
  *how loudly* it degrades; it does not decide whether the fallback tier gets
  encryption. That stays with #4570.

### 1.3 Verified current state — what this replaces

Measured on `origin/main` (`11ef5c27`), production source only
(`crates/*/src/**`, tests excluded). Every row was re-verified for this document,
not copied from the epic:

| Claim | Verified |
|---|---|
| There is no principal concept and no ACL in the credential path | `resolve_key(provider)` (`crates/trusty-common/src/inference/credentials/resolver.rs:79`) takes a provider name **and nothing else**. Any code in any crate that can call it gets any credential the process can see. That is the entire access-control model today. |
| The OS keychain is compiled by nobody | `keyring-store` is defined at `crates/trusty-common/Cargo.toml:561` and enabled by **no** crate, workflow, or Makefile target; `default = []` (line 266). So `default_store()` (`resolver.rs:129`) never takes its `#[cfg(feature = "keyring-store")]` branch and falls straight through to `FileKeyStore` — `0600`, **plaintext**, at `~/.trusty-tools/credentials.toml`. |
| `credential_ref` does not exist | **Zero** hits under `crates/*/src/**`, despite being an acceptance bullet of epic #2808 (closed `COMPLETED`). It appears only in `docs/specs/trusty-agents-eve-style-agents-spec.md` and one research doc. |
| MCP secrets sit in plaintext in a user-editable file | `McpService.env: HashMap<String, String>` (`crates/trusty-agents/src/mcp/config/types.rs:181`), whose own doc comment says *"e.g. API keys"*, serialised into `~/.trusty-agents/config.toml`. There is no reference type, so there is nothing else it could be. |
| There is no credential audit trail anywhere | `resolve_key` records nothing. The only audit machinery in the workspace is `phase_audit` (agent phases), `registry_search_audit` (trusty-code search), and `audit_palaces` (memory doctor) — none of them credentials. |
| The registry covers a minority of this repo's credentials | `env_var_for` (`resolver.rs:44`) maps **10** providers. A census of production source finds **23 distinct credential env-var names** across **36 files**. 13 are unmapped, so no consumer *can* route them through the resolver even if it wanted to. |
| The reachable-set floor is fail-closed, and this design matches it | `ASSISTANT_REACHABLE_SUBAGENTS` (`crates/trusty-agents/src/agents/delegation.rs:78`) is `["research-agent", "ticketing-agent"]`; `build_assistant_tier_registry`'s doc states *"`None` reaches nothing — fail-closed"* (`crates/trusty-agents/src/runtime/tool_registry.rs:549`). §5's default-deny is the same posture applied to credentials. |
| The crate home is determined by the dependency graph, not preference | `trusty-agents-common` has **zero** `trusty-*` dependencies — a deliberate leaf breaking a cargo cycle for the plugin API surface — so it structurally cannot reach the existing resolver. `trusty-common` is the only crate every consumer already depends on, and it already hosts `env_var_for`. `trusty-agents` reaches `credentials` transitively today via `inference-client = ["credentials"]` (`crates/trusty-common/Cargo.toml:573`). |

---

## 2. Scope, Non-Goals, and Terms

### 2.1 Scope

The authority: principal identity, credential identity, ACL and scoping,
default-deny, the delegation boundary, denial diagnostics, revocation and expiry,
the audit record, secure delivery, at-rest storage, and the sub-agent /
project-trust boundary. Cross-product — **trusty-agents and trusty-code** — per
the owner's 2026-08-01 scheduling decision.

### 2.2 Non-goals

- **Any capability grant.** Stated twice on purpose (§1). This epic ships
  authority; it grants nothing new.
- **#4439 Constraint 1** — authority-holder gating of computer-use actions
  (DOC-41 §5.5, #3074). See §12.4 for the seam.
- **The command dimension of routed shell.** #4550 owns it. See §13.
- **The migration of 55 raw `std::env::var` reads.** #4571 owns it. §5.6 states,
  as an honesty clause, what that means for the strength of default-deny until it
  lands.
- **Encrypting the fallback file store.** #4570 / #4034 Slice 7.

### 2.3 Terms

| Term | Meaning in this document |
|---|---|
| **Credential** | A secret that authenticates to a provider: API key, OAuth access/refresh token, App private key, webhook signing secret, bearer, cookie. |
| **`CredentialRef`** | The opaque, **non-secret** handle that *names* a credential. §4. |
| **`Secret<T>`** | The wrapper a resolved credential is returned in, whose `Debug`/`Display` never render the value. |
| **Principal** | The thing an authorization decision is made *about*. §3. Always **derived**, never self-declared. |
| **Grant** | A `(Principal, CredentialRef, Scope)` triple, optionally bounded by an expiry. §5. |
| **Scope** | What a grant permits: at minimum read-vs-write, plus provider-native scopes where the provider expresses them. §5.4. |
| **Authority** | The component in `trusty_common::credentials` that owns grants, resolution, revocation state, and the audit stream. |
| **Resolution** | `resolve(ref, principal, requested_scope)` — the single entry point. §5.2. |

### 2.4 Normative language

**MUST**, **MUST NOT**, **SHALL**, **SHOULD**, and **MAY** carry their ordinary
RFC-2119 force. Clauses are numbered `C-<section>.<n>` and are the citable unit;
a consumer issue that says "unblocked by `C-6.2`" means that clause.

A clause marked **PROVISIONAL** depends on an unanswered owner question named in
§14 and **MUST NOT** be implemented as written until that question is answered.

---

## 3. SPEC-CREDAUTH-01 — Principal Identity {#SPEC-CREDAUTH-01~draft}

**ID:** `SPEC-CREDAUTH-01~draft`
**Status:** Draft — `C-1.3` PROVISIONAL pending owner **Q-B**.

#4563 lists five candidates present in the codebase today and asks which are
distinct principals and which collapse. **Four are distinct; one collapses.**

### 3.1 The four principal kinds

**C-1.1** The authority SHALL recognise exactly four principal kinds:

| Kind | Identity | Why it is distinct |
|---|---|---|
| **Operator** | The human at the keyboard / the authenticated owner of the machine. | Already a real, enforced discriminator: `user_authority` exists and is checked in 10 source files (`crates/trusty-agents/src/agents/permissions.rs`, `api/server/agent_permissions.rs`, `tools/cross_product.rs`) — but it gates *tools*, never credentials. #3076 wanted exactly this seam for a `user.*` credential namespace and died without an answer. It is the highest authority and the only principal that may *issue* and *revoke* grants (`C-3.6`). |
| **Assistant** | `AssistantInstanceId` (`crates/trusty-agents/src/assistants/instance.rs:55` — a validated newtype, PR #4523). | The unit that has a persona, a home, a config, and an untrusted-input surface. It is the thing an operator thinks of when deciding "may *this* assistant read my Gmail token". |
| **SubAgent** | The pair `(sub-agent name, delegating AssistantInstanceId)`. | The load-bearing consequence of §6. A sub-agent must be a **distinct** principal from its delegator, or "hold your own grant" is unexpressible. Keyed on the *pair*, not the name alone, so `izzie`'s `version-control` and `cto-assistant`'s `version-control` are separately grantable and separately revocable. |
| **Service** | A stable `ServiceId` for a daemon or non-assistant process (`trusty-search`, `trusty-memory`, `trusty-analyze`, the `tm` daemon, a `trusty-code` session). | Required by the owner's cross-product decision. **trusty-code has no assistant in the loop at all**; without a service principal the model would not apply to half its declared scope. |

**C-1.2** A principal SHALL be **derived by the authority from the executing
context**, never declared by the thing it names. An assistant MUST NOT be able to
construct, assert, or influence its own `Principal`, and no file the assistant can
edit — its persona TOML, anything under its home — may change it. This mirrors the
server-owned-floor property ADR-0024 decision 4 established for
`ASSISTANT_REACHABLE_SUBAGENTS` and enforces server-side in `agent_patch.rs`. A
self-attested principal is not a principal.

**C-1.3 — PROVISIONAL (owner Q-B).** The `Assistant` kind's identity is
`AssistantInstanceId`, i.e. **one namespace per instance**: `izzie` and
`cto-assistant` resolve against separate grant sets even though both are
instances of the same persona type. This is the fail-closed reading and the one
consistent with #4523's per-instance homes — but the owner has not ruled, and the
alternative (one namespace per persona *type*, so two instances share) is a
coherent product answer with materially less configuration burden. **Do not
implement `Principal`'s `Assistant` variant until Q-B is answered.** Everything
else in this section is settled regardless of which way Q-B goes.

### 3.2 The collapse: session is not a principal

**C-1.4** A **session** SHALL NOT be a principal. It collapses into the
`Assistant` (or `Service`) principal that owns it for every authorization
decision.

Why: a session is a *conversation lifetime*, not an authority holder. Making it a
principal would require a grant to be re-issued per session — the "asks for the
credential again every restart" failure mode — and there is no session-scoped
grant surface anywhere in the codebase to hang it on.

**C-1.5** The session identifier SHALL nonetheless be carried on every audit
record (§9). The forensic question is almost always *"which conversation caused
this read"*, and losing it would make the trail materially less useful for no
authorization benefit. **Audit attribute, not authorization key** is the general
shape, and it recurs in `C-1.6`.

### 3.3 Invocation identity

**C-1.6** A sub-agent **invocation** SHALL NOT be an authorization key.
Authorization keys on the `(name, delegator)` pair (`C-1.1`), which is stable and
therefore grantable; an invocation is ephemeral and cannot be granted to before
it exists. The invocation identifier SHALL appear on the audit record so two
calls by the same sub-agent are distinguishable in the trail.

### 3.4 Principal in the type system

**C-1.7** `Principal` SHALL be a closed enumeration in
`trusty_common::credentials`, not a string. A stringly-typed principal would
re-open `C-1.2` — any caller could spell one — and would make the exhaustive
match that `C-3.2`'s floor depends on impossible to write.

**C-1.8** `Principal` SHALL render its full shape in `Debug`/`Display` — it
carries no secret material by construction, which is what makes it safe to place
on every audit record (§9) without a redaction pass.

---

## 4. SPEC-CREDAUTH-02 — Credential Identity: `CredentialRef` {#SPEC-CREDAUTH-02~draft}

**ID:** `SPEC-CREDAUTH-02~draft`
**Status:** Draft. Satisfies DOC-63 `S-5.7` and §7.1b requirement 1.

### 4.1 What it is

**C-2.1** A `CredentialRef` SHALL be an **opaque, durable, serialisable,
non-secret handle** that *names* a credential without carrying it. It is safe to
write into a git-tracked file, a config TOML, a store row under the assistant
home, a log line, an audit record, and a model-visible `ToolResult`.

**C-2.2** A `CredentialRef` SHALL be **stable across rotation**. Rotating the
underlying secret MUST NOT change the ref. This is what makes DOC-63 `S-5.7`'s
store row *durable*: a row written once keeps working across every rotation, and
rotation never becomes a config-editing exercise across N files.

**C-2.3** A `CredentialRef` names a **credential**, not a grant. The grant is the
`(Principal, CredentialRef, Scope)` triple of §5. Conflating them would force one
ref per principal and make a shared ref — the whole point of a store row that
several principals may be separately granted against — inexpressible.

### 4.2 Non-secrecy by construction, not by discipline

**C-2.4** `CredentialRef` SHALL have a **restrictive grammar** — a bounded
character set and length that no realistic API key, JWT, OAuth token, or PEM body
can satisfy. Constructing one from text that does not match SHALL be a
recoverable parse error.

This is deliberately stronger than "don't put secrets in refs". #4567 requires the
audit record to accept a `CredentialRef` rather than a `String` precisely so that
secret material is *unrepresentable*; that guarantee is only worth as much as the
ref type's own grammar. A `CredentialRef(String)` newtype with no validation
would launder an arbitrary string straight into the audit stream and defeat DOC-63
`S-5.8`.

**C-2.5** `CredentialRef` SHALL be **shape-agnostic**. The same type names an
OAuth credential (Gmail, Drive, Notion, Slack) and a plain API key (Granola, and
Fireflies later). DOC-63 §7.1b item 5 warns that the API-key shape is the one most
likely to be special-cased, and states that it must not be. There SHALL be exactly
one entry point (`C-3.3`) through which both shapes resolve, and no type, trait,
or code path that exists only for one of them.

**C-2.6** `Display` for `CredentialRef` SHALL render the ref verbatim. It is
non-secret by `C-2.1` and `C-2.4`; masking it would only make the audit trail
harder to read while buying nothing.

### 4.3 What it points at

**C-2.7** A `CredentialRef` SHALL resolve through the **provider registry**
(`env_var_for` and its successor, #4564) so that a ref, a registry entry, and a
storage location are one chain rather than three parallel ones. A ref naming a
provider absent from the registry SHALL fail with `Missing` (`C-5.1`) carrying a
remediation string that names the registry — the failure mode 13 of today's 23
credential env-vars would otherwise hit silently.

---

## 5. SPEC-CREDAUTH-03 — Grants, Scoping, and Default-Deny {#SPEC-CREDAUTH-03~draft}

**ID:** `SPEC-CREDAUTH-03~draft`
**Status:** Draft.

### 5.1 The grant

**C-3.1** A grant SHALL be the triple `(Principal, CredentialRef, Scope)`,
optionally bounded by an expiry instant. Nothing else confers the ability to
resolve a credential.

### 5.2 Default-deny — the inversion

**C-3.2** An **unlisted** `(Principal, CredentialRef)` pair SHALL resolve to
`Denied` (`C-5.2`). It SHALL NOT resolve to the credential, and it SHALL NOT fall
through to the process environment, `.env.local`, or the key store.

This is the inversion. Today the default is allow-everything-in-process:
`resolve_key(provider)` consults `std::env::var` for any caller in any crate.
Under `C-3.2` the three storage tiers become *where a value lives*, never *who may
read it*.

**C-3.3** Resolution SHALL have exactly one entry point, shaped
`resolve(&CredentialRef, &Principal, Scope) -> Result<Secret<…>, CredentialError>`.
A second resolution path is a defect under this repo's common-entry-point rule and
under DOC-63 `S-5.2`, and is rejected at review regardless of expedience.

**C-3.4** Where a provider needs one, the authority SHALL be able to hand back an
**already-authenticated client handle** rather than a bare string, so a source
type receives *"a resolved, scoped client and nothing else"* — DOC-63 `S-5.1`,
restating DOC-55's connector obligation C8. A caller that never sees the string
cannot leak the string.

### 5.3 Where a grant lives, and the floor that config cannot widen

**C-3.5** Grants SHALL be stored **outside the assistant home** and outside any
project tree. A grant declared in a file under `${TRUSTY_AGENTS_HOME}/<agent>/`,
in a persona TOML, or in a project-local config SHALL be ignored with a warning,
not honoured. DOC-63 `S-5.6` forbids secrets in the home; a *grant* is not a
secret, but a grant an assistant can edit is a grant an assistant can widen, and
that is the same defect one level up.

**C-3.6** Only the **Operator** principal may issue or revoke a grant.

**C-3.7** Effective reach SHALL compose as:

```
effective(principal) = floor(principal_kind) ∩ configured_grants(principal)
```

where `floor` is **code-owned** and config can only **narrow** it. This is
ADR-0024 decision 4's ratified shape, applied to credentials rather than to
sub-agent reachability, and it is why `C-1.7` requires a closed enumeration — the
floor is an exhaustive match over principal kinds. A configuration edit MUST NOT
be able to widen reach, and this MUST be test-pinned in the style of
`bundled_personas_pin_git_reach`.

### 5.4 Scope

**C-3.8** `Scope` SHALL carry, at minimum, a read/write dimension, and SHALL be
able to carry provider-native scopes where the provider expresses them (OAuth
scope strings).

**C-3.9** Resolution SHALL be **scope-checked at the point of resolution**, not
at the point of use. `resolve` takes the requested scope; a grant that does not
cover it returns `ZeroScope` (`C-5.4`) before any secret is materialised.

**C-3.10 — read-scoped grants, honestly.** Where a provider *can* issue a
read-scoped credential, a read-only consumer SHALL be issued one. Where a provider
*cannot*, the authority SHALL return `ScopeUnavailable` (`C-5.5`) — a
machine-readable answer the caller can surface — and SHALL NOT silently hand back
a write-capable credential.

This clause exists because the false version of it has already been shipped and
caught. The base persona's own comment
(`crates/trusty-agents/.trusty-agents/agents/assistant/agent.toml:105-113`)
records that today's Google-backed ingest tools reuse **write-capable** OAuth
grants and return `None` from `ToolExecutor::scope()` — so they are **not
scope-gated at all** — after a read-only claim had previously been made and been
false. DOC-63 `S-5.3` calls this out by name. Do not repeat it.

### 5.5 Every failure is recoverable

**C-3.11** Every resolution failure SHALL be a recoverable `Result::Err`. No
panic, no `unwrap`, no empty-string-as-success, and no silent `None` that a caller
can mistake for "no credential configured" when the real answer was "you were
denied".

### 5.6 Honesty clause — what default-deny does NOT yet buy you

**C-3.12** Until #4571 lands, **`C-3.2` is not airtight, and this document does
not claim it is.** 55 raw `std::env::var("<CREDENTIAL>")` reads across 36 files in
9 crates bypass the resolver entirely; every one of them is a residual path by
which in-process code reads a credential without a principal, a grant, or an audit
record. The authority governs the paths that route through it, and #4571 is what
makes "the paths that route through it" mean "all of them".

Implementations MUST NOT describe default-deny as complete before that ticket
lands, and SHOULD ship a lint or grep-based gate that prevents a **56th** raw read
from being added — the counts have grown materially since the 2026-07-11 baseline
(`OPENROUTER_API_KEY`: 22 files → 105; `GITHUB_TOKEN`: 13 → 59), and an
un-ratcheted migration loses to new code.

---

## 6. SPEC-CREDAUTH-04 — The Delegation Boundary {#SPEC-CREDAUTH-04~draft}

**ID:** `SPEC-CREDAUTH-04~draft`
**Status:** Draft. **Owner decision, 2026-08-01 — settled, not open.**

> A credential grant does not survive delegation. A sub-agent must hold its own
> grant, checked against its own principal — an assistant cannot lend the reach it
> holds. This is fail-closed, consistent with `delegate_allowed` resolving absent
> config to the empty set.
>
> — Owner, on #4563 (its Q4) and #4040, 2026-08-01

Rejected, and recorded in [ADR-0026](../adr/0026-credential-grants-do-not-survive-delegation.md):
**inherit-narrowed** (implicit inheritance is harder to audit than an explicit
per-principal grant) and **inherit-unchanged** (would flow an untrusted-input
persona's reach straight to whatever it delegates to).

### 6.1 Non-transitivity

**C-4.1** A grant SHALL NOT be transitive. `resolve` SHALL be evaluated against
the **calling** principal, always. There SHALL be no delegator fallback, no
ambient credential context, and no "resolve as my parent" parameter.

**C-4.2** `SubAgent { name, delegator }` (`C-1.1`) is a **distinct principal**
from `Assistant(delegator)`. A grant to the latter confers nothing on the former.

**C-4.3** A sub-agent with no grant of its own SHALL receive `Denied` (`C-5.2`),
not the delegator's credential and not a fallback. Fail-closed, matching
`build_assistant_tier_registry`'s ratified posture that *"`None` reaches nothing"*
(`crates/trusty-agents/src/runtime/tool_registry.rs:549`).

### 6.2 Secret-passing is forbidden — the clause without which §6 is decorative

**C-4.4** A resolved `Secret` SHALL NOT cross a delegation hop. A delegator MUST
NOT pass a resolved credential to a sub-agent as a task parameter, a prompt
fragment, a `HandoffContext` field, an environment overlay, or a file.

Without `C-4.4`, "hold your own grant" is bypassed in one line —
`delegate_to_agent(task: "run gh with token sk-…")` — and the boundary is
theatre. This SHALL be enforced structurally where the type system permits: a
delegation payload type MUST NOT be constructible from a `Secret`, mirroring
#4565's requirement that a resolved credential never be returned from a function
whose result is `Serialize` into a `ToolResult`.

### 6.3 There is no delegator-derived ceiling — and this is deliberate

**C-4.5** A sub-agent's effective reach SHALL NOT be capped by its delegator's.
`effective(SubAgent{name, delegator})` is computed by `C-3.7` from the sub-agent's
own principal alone, and is **not** intersected with `effective(Assistant(delegator))`.

This is the clause most likely to be "helpfully" added during implementation, and
adding it would break the directive it is meant to protect. #4417 routes git
through a `version-control` sub-agent **precisely so the assistant need not hold
git reach**. If the sub-agent's GitHub grant were capped by the assistant's — which
is empty, by design — routing would resolve to nothing and the directive would be
unimplementable.

A ceiling is not the same mechanism as inheritance (a ceiling only denies,
inheritance grants), so it is not what the owner rejected; it is rejected here on
its own merits, for the reason above.

**Note for #4566.** #4566's acceptance bullet *"Delegation narrows: a sub-agent
principal derived from a delegator resolves a strict subset; property-tested"* was
written **before** the owner's Q4 answer and describes **inherit-narrowed**, the
rejected alternative. Under `C-4.1`/`C-4.5` there is nothing to narrow: a
sub-agent's set is independently granted, and whether it happens to be a subset is
a configuration outcome, not an invariant. That bullet needs replacing with a test
of `C-4.2` + `C-4.3` (a sub-agent resolves `Denied` for a credential its delegator
holds) and `C-4.4`.

### 6.4 The cost, stated plainly

**C-4.6** This design **increases configuration burden** and that is the point.
Routing git through `version-control` (#4417 / #4479) now requires the operator to
grant that sub-agent its own GitHub credential; routing shell through Computer Use
(#4439) requires the same for whatever it reaches. Each such grant becomes an
explicit, listed, auditable, individually revocable row rather than an implicit
consequence of who called whom.

Implementations SHOULD make the first denial actionable: `Denied`'s remediation
string (`C-5.7`) SHALL name the principal and the ref, so the operator's next
action is a single grant command rather than a debugging session.

---

## 7. SPEC-CREDAUTH-05 — The Denial Taxonomy {#SPEC-CREDAUTH-05~draft}

**ID:** `SPEC-CREDAUTH-05~draft`
**Status:** Draft.

#4040's "Done when" names four diagnostics that must be distinguishable. This
document specifies those four **plus a fifth**, and states why the fifth is not a
sub-case of any of them.

### 7.1 The variants

**C-5.1 — `Missing`.** No credential is stored for this `CredentialRef` at all.
The grant may well exist; there is simply nothing to resolve. *Operator action:
store the credential.*

**C-5.2 — `Denied`.** The credential exists, but this principal holds no grant
covering it. This is the default-deny answer (`C-3.2`) and the sub-agent answer
(`C-4.3`). *Operator action: issue a grant, or accept the denial.*

**C-5.3 — `Expired`.** A grant or credential existed and is no longer valid —
either the grant's expiry instant has passed or the authority has recorded the
credential as revoked/expired (§8). *Operator action: re-authenticate or re-grant.*

**C-5.4 — `ZeroScope`.** The grant exists and is live, but its scope does not
cover the requested scope — the intersection is empty. *Operator action: widen the
grant's scope.*

**C-5.5 — `ScopeUnavailable`.** The **provider** cannot issue a credential at the
requested scope at all. Not a property of the grant; a property of the provider.

**C-5.6** `ScopeUnavailable` SHALL be a distinct variant and SHALL NOT be folded
into `ZeroScope`. They are different facts with different remediations, and
conflating them re-creates exactly the dishonesty DOC-63 `S-5.3` exists to
prevent: `ZeroScope` tells an operator *"widen the grant"*, which for a provider
with no read-only scope is advice that cannot be followed and will be worked
around by granting write. `ScopeUnavailable` is the honest, machine-readable
*"this provider has no such scope"* that #4531 needs in order to display the
truth rather than imply a read-only grant that does not exist.

**This is an addition to #4040's stated four**, made deliberately and flagged
here rather than folded in silently. #4566's acceptance criteria should be
updated to five.

### 7.2 Properties of every variant

**C-5.7** Every variant SHALL carry an **actionable remediation string** naming
the principal and the `CredentialRef`. `Denied` in particular SHALL name both, so
that the first denial a sub-agent hits under §6 tells the operator exactly which
grant to issue (`C-4.6`).

**C-5.8** Every variant SHALL be **constructible and observable from a test**, and
distinguishable by a caller through pattern-matching — not through string
comparison on a rendered message.

**C-5.9** No variant SHALL carry secret material. A remediation string references
the `CredentialRef` (`C-2.1`), never a value, never a partial value, and never a
prefix "for identification".

**C-5.10** `Missing` and `Denied` SHALL be **distinguishable to the operator but
must not be distinguishable to an untrusted caller in a way that becomes an
enumeration oracle.** Where a principal's own denial message is surfaced into
model-visible context, the surfaced form SHALL NOT reveal whether a credential the
principal is not granted actually exists. The full distinction lives in the audit
record (§9), which the operator reads and the model does not.

---

## 8. SPEC-CREDAUTH-06 — Revocation and Expiry as an Observable Signal {#SPEC-CREDAUTH-06~draft}

**ID:** `SPEC-CREDAUTH-06~draft`
**Status:** Draft. Satisfies DOC-63 `S-5.4` and §7.1b requirement 4.

### 8.1 The requirement

DOC-63 `S-5.4` requires a consumer to learn that a credential died **from the
authority**, not by inferring it from a 401. *"A scheduled source silently failing
forever on an expired token is the failure mode this clause exists to prevent."*

### 8.2 State, not inference

**C-6.1** The authority SHALL hold an observable **state** per
`(Principal, CredentialRef)`, at minimum: `Live`, `Expired`, `Revoked`,
`NeedsReauth`.

**C-6.2** A consumer SHALL be able to **query** that state without attempting a
resolution, and SHALL be able to **subscribe** to transitions. DOC-63 `S-5.4`'s
displayed `needs-credential` state and its schedule stop are driven by this
signal.

**C-6.3** A revoked or expired grant SHALL produce `Expired` or `Denied`
**before any network call is attempted**. The authority must not delegate the
detection of its own revocations to the provider.

### 8.3 Two kinds of death, and the honest handling of the second

**C-6.4** The authority SHALL distinguish:

- **Grant revocation** — an operator action against a `(Principal, CredentialRef)`
  row. Local, authority-side, immediate, and fully knowable in advance.
- **Credential expiry / upstream revocation** — the provider invalidated the
  secret. The authority knows this in advance **only** when the credential
  declared a lifetime.

**C-6.5** For the second kind with no declared lifetime, the authority SHALL
expose a **report channel**: a consumer that receives a provider rejection
(401/403 or the provider's equivalent) SHALL report it to the authority against
the `CredentialRef`, and the authority SHALL transition the state and notify
subscribers.

This is the clause that makes `S-5.4` achievable rather than aspirational. The
authority cannot always know about an upstream revocation proactively — no design
can. What it can do is ensure that the **first** 401 is a *report*, and that every
subsequent consumer learns from the authority instead of discovering it
independently. "Learned from the authority, not inferred from a 401" is a property
of the system, not a claim that no 401 is ever observed.

**C-6.6** A report SHALL carry only the `CredentialRef` and a provider-status
discriminator. It SHALL NOT carry the response body, the request headers, or the
URL — §9's prohibitions bind this path too, and a rejection response is a
plausible carrier for a token echo.

### 8.4 Rotation

**C-6.7** Rotating a credential SHALL NOT invalidate grants, because a
`CredentialRef` is stable across rotation (`C-2.2`). Rotation transitions state
back to `Live` and notifies subscribers; no store row, config file, or grant is
edited.

---

## 9. SPEC-CREDAUTH-07 — The Audit Trail {#SPEC-CREDAUTH-07~draft}

**ID:** `SPEC-CREDAUTH-07~draft`
**Status:** Draft — **§9.3 and §9.5 are PROVISIONAL pending owner Q-A.**

Without this section #4040's "Done when" clause *"access is attributable and
revocable"* cannot be satisfied: *revocable* arrives with §8, *attributable*
arrives here. There is **no credential audit trail anywhere in this workspace
today** (§1.3).

### 9.1 The record — settled regardless of Q-A

**C-7.1** An audit record SHALL carry:

| Field | Notes |
|---|---|
| timestamp | UTC instant |
| principal | The full `Principal` (`C-1.8`), including the delegator for a `SubAgent` — otherwise a cross-hop denial is unattributable |
| credential | The `CredentialRef`. **Never a value.** |
| requested scope | What was asked for |
| decision | Allow or deny |
| denial variant | One of `C-5.1`–`C-5.5` where the decision was deny |
| call site | Enough to locate the caller |
| session id | `C-1.5` — audit attribute, not authorization key |
| invocation id | `C-1.6`, where the principal is a `SubAgent` |
| project context | The active project, and the target project where they differ (`C-10.4`) |

**C-7.2** A record SHALL be emitted on **every** resolution attempt, allowed and
denied alike. A denial is the more interesting forensic event and is the one most
likely to be dropped by an implementation optimising for the happy path.

### 9.2 Prohibitions — settled, and non-negotiable

**C-7.3** The record type SHALL make secret material **unrepresentable by
construction**: it accepts a `CredentialRef` (`C-2.1`, with `C-2.4`'s restrictive
grammar), never a `String` that might hold a value. It SHALL NOT rely on
redaction of arbitrary text.

**C-7.4** No record SHALL contain a credential, a partial credential, a request
header, or a URL query string. This restates DOC-63 `S-5.8` in force: *"neither
may record a credential, and neither may echo a request header or URL query string
that could carry one."*

**C-7.5** This is not advisory. **The audit stream is the one place where a bug
converts a security control into a secret-disclosure channel** — a stream designed
to be retained, read, and possibly shipped. #4521 reaches the same conclusion
independently for shell command text: redaction heuristics on arbitrary text are
unreliable, so the type must not accept arbitrary text.

**C-7.6** A test SHALL assert that a resolved secret's bytes never appear in a
captured audit stream.

### 9.3 PROVISIONAL — cadence (owner Q-A)

**C-7.7 — PROVISIONAL.** The default is **one record per resolution call**.
Aggregation (per session, or per grant) is the alternative the owner has not ruled
on. Per-call is the fail-safe default — it cannot lose an event — and it is what
`C-7.2` reads most naturally. It is also the one with a real cost: a tight
resolution loop produces a high-volume stream.

**Do not implement aggregation, and do not implement a retention policy that
assumes aggregation, until Q-A is answered.** #4567's acceptance criteria depend
on this answer.

### 9.4 Routability — settled

**C-7.8** The stream SHALL be emitted on its own `tracing` target (e.g.
`trusty_common::credential_audit`) so an operator can route it to appropriate
retention and access, **or suppress it, without silencing the rest of the crate's
logging.** A test SHALL assert that suppressing it does not suppress the crate's
other logging.

### 9.5 PROVISIONAL — topology and retention (owner Q-A)

**C-7.9 — PROVISIONAL.** Three audit streams are now in play and **none of them
exists yet**:

| Stream | Records | Owner |
|---|---|---|
| credential access | principal, `CredentialRef`, allow/deny | **this document** / #4567 |
| shell execution | command, cwd, exit code | #4521 |
| permission decision | allow / ask / deny outcome | #4550 |

Whether these are **one stream with a discriminator** or **three independently
routable ones** is owner Q-A and is not decided here. What this document does fix,
so the three cannot drift whichever way Q-A goes:

**C-7.10** Every one of the three SHALL obey `C-7.3`–`C-7.5`. Whatever the
topology, no stream may accept arbitrary text where a typed, non-secret value
would do.

**C-7.11** Every one of the three SHALL be independently suppressible (`C-7.8`).
A single-stream answer therefore requires per-category filtering, not a single
on/off switch.

**C-7.12 — PROVISIONAL.** Retention defaults to a **bounded local sink** — capped
by size and age, written under `~/.trusty-tools/`, never under the assistant home
(`C-8.6`) and never under a project tree. Consistent with the loopback-only
doctrine (ADR-0018), records are **not shipped off-host** by default. The exact
cap and the shipping question are part of Q-A.

---

## 10. SPEC-CREDAUTH-08 — Delivery, and the Assistant-Home Prohibition {#SPEC-CREDAUTH-08~draft}

**ID:** `SPEC-CREDAUTH-08~draft`
**Status:** Draft. Discharges the design half of the now-closed #3038 (which
originally claimed DOC-45). Satisfies DOC-63 `S-5.6` and §7.1b requirement 2.

### 10.1 The universal prohibition

**C-8.1** A resolved credential SHALL NOT be written into: a git-tracked file, a
project-local file, any file under `${TRUSTY_AGENTS_HOME}`, a `ToolResult`,
model-visible context, an ordinary log line, an audit record, or a URL.

**C-8.2** `Secret<T>`'s `Debug` and `Display` SHALL never render the wrapped
value, property-tested such that the rendered form contains no substring of it.
`crates/trusty-common/src/inference/types/secret.rs` already exists; #4565 extends
or folds it in rather than writing a second one.

**C-8.3** A resolved credential SHALL NOT be returned by value from any function
whose result is `Serialize`d into a `ToolResult`. Where the type system permits,
this is pinned at compile time rather than by review.

### 10.2 The three delivery channels

**C-8.4** Resolution happens **at use time**, never at config-load time and never
at spawn-list-construction time. The window in which a secret exists in memory is
bounded by the call that needs it.

| Channel | Mechanism | Constraint |
|---|---|---|
| **stdio subprocess** (MCP servers, `gh`, `git`) | Injected into the child's environment at spawn, from the resolved `Secret`. | Never written back into the config that produced the spawn (#4568). The parent's own environment is **not** inherited wholesale — see `C-11.4`. |
| **HTTP request** (remote MCP, provider APIs) | Injected into the request header at call time. | The header SHALL be excluded from every logging, tracing, capture, and error-reporting path. A `url` containing what looks like a credential is **rejected, fail-closed** (#3038's URL check, carried by #4568). |
| **remote / fleet session** | Delivered over the existing authenticated channel at use time. | Never materialised into the remote host's filesystem. Consistent with the loopback-only doctrine (ADR-0018): the delivery channel is the authenticated one that already exists, not a new listener. |

**C-8.5** Both credential shapes (`C-2.5`) SHALL traverse all three channels
through the same code path. There SHALL be no OAuth-only or API-key-only delivery
mechanism.

### 10.3 The home prohibition, as a structural impossibility

DOC-63 `S-5.6` is *"the single most important clause in this section"*: no
credential is ever written into `${TRUSTY_AGENTS_HOME}/<agent>/`. The reason is
structural — #4325 designs that home to be opened, read, and hand-edited, and
#4523 implements exactly that, dotless and browsable. PR #4523's own closing note
asks this spec to name a separate credential store. #4563's acceptance criteria
require this be stated *"as a prohibition rather than a preference"* and made
*"structurally impossible, not merely discouraged"*.

**C-8.6** The credential store's root SHALL be `~/.trusty-tools/` (today's
`FileKeyStore` location) and the grant store's root SHALL be alongside it. Neither
SHALL be relocatable to a path under `${TRUSTY_AGENTS_HOME}` or under any project
tree.

**C-8.7** Store construction SHALL perform a **path containment check** and SHALL
**fail at construction** — not at write time — if the resolved store path lies
under `${TRUSTY_AGENTS_HOME}` or a registered project root. A configuration that
would place secrets in the browsable home is a startup error, not a runtime
warning.

**C-8.8** A store row, config row, or state file under the home carries a
`CredentialRef` (`C-2.1`) and never a credential. This is the mechanism that makes
`C-8.1` true by construction rather than by everyone remembering it — and it is
what does not exist today, since `credential_ref` has zero hits in the workspace
and `McpService.env` is a `HashMap<String, String>` documented as holding API keys.

---

## 11. SPEC-CREDAUTH-09 — At-Rest Storage {#SPEC-CREDAUTH-09~draft}

**ID:** `SPEC-CREDAUTH-09~draft`
**Status:** Draft. **Owner decision, 2026-08-01 — this answers #4040's Q1 and
unblocks #4570.**

### 11.1 The decision

> Keyring default, plaintext fallback, and **log loudly** when it falls back.
>
> — Owner, 2026-08-01

**C-9.1** `keyring-store` SHALL be enabled **by default** in `trusty-common`. It
is defined today (`crates/trusty-common/Cargo.toml:561`) and enabled by nobody, so
`KeyringStore` is dead code in every shipped binary and **every credential this
repo stores at rest is plaintext.**

**C-9.2** `default_store()` SHALL select `KeyringStore` when the OS keychain is
reachable, and fall back to the `0600` `FileKeyStore` when it is not — headless,
SSH, CI.

**C-9.3** The fallback SHALL **log loudly**: at `warn` or higher, on a target the
default configuration surfaces, naming both the reason (no reachable keyring) and
the consequence (credentials stored in plaintext at `~/.trusty-tools/credentials.toml`).
It SHALL be emitted at least once per process. **The weaker mode is never
silent.**

**C-9.4** The fallback SHALL NOT hard-fail. This is the constraint #3066 and #3076
both foundered on: the Keychain is unavailable over SSH and headless, and
*bypassing it entirely was a shipped fix for a real breakage* (#1551 / #2246, per
#4034's recorded constraints). This repo is routinely driven over SSH. A hard-fail
would re-break the case that fix closed.

**C-9.5** `MemoryKeyStore` remains the last resort for the case where
`dirs::home_dir()` itself fails (CI, containers), and SHALL log at the same
loudness as `C-9.3`.

### 11.2 What this does and does not settle

**C-9.6** This clause settles **which backend** and **how loudly it degrades**. It
does **not** settle whether the fallback file tier should be encrypted (#4034's
Slice 7, never decomposed), nor whether a privileged `user.*` namespace should be
held to a stricter rule than the rest (#3076's proposal). Both stay with #4570,
which this section otherwise unblocks.

**C-9.7 — honesty.** Under `C-9.2`, on a headless or SSH host the at-rest posture
is `0600` plaintext. Documentation and diagnostics SHALL say so plainly rather
than describing the product as "keychain-backed" — that description is true of a
desktop session and false of the deployment shape this repo is most often driven
in.

### 11.3 Storage is not authorization

**C-9.8** The storage tier SHALL NOT be an authorization bypass. The three-tier
precedence (process env → `.env.local` → `KeyStore`) determines **where a value
lives**; §5's ACL determines **who may read it**. A credential present in the
process environment is subject to `C-3.2` exactly as one in the keychain is.

---

## 12. SPEC-CREDAUTH-10 — The Sub-Agent and Project-Trust Boundary {#SPEC-CREDAUTH-10~draft}

**ID:** `SPEC-CREDAUTH-10~draft`
**Status:** Draft. **This is the section #4417, #4479, and #4439 Constraint
2(b)(c)(d) each independently asked for.** All three want the same three
properties, and none of them is a library — they are design properties, which is
why #4563 alone unblocks them.

### 12.1 The shape being closed

Stated by #4417 and restated by #4479: routing git through a sub-agent **moves the
trust boundary without closing it**. An L0 assistant that ingests untrusted content
(Gmail, Drive, Calendar) can simply *ask* the sub-agent to read an arbitrary
registered project, and the same information flows back to the same
untrusted-input persona. §6 closes the *credential* half. This section closes the
*project* half.

### 12.2 (b) — What filesystem/project scope a routed call gets

**C-10.1** A `SubAgent` principal SHALL carry a **project scope**: the set of
registered projects it may operate on. Project scope is a dimension of the grant
(`C-3.1`), issued to the sub-agent's own principal, and is subject to `C-3.7`'s
code-owned floor.

**C-10.2** The default project scope SHALL be **the delegator's active project
only**. Absent an explicit grant, a routed call outside that project SHALL be
denied.

**C-10.3 — interpretation, flagged.** Asked whether the sub-agent may reach any
registered project or only the active one, the owner answered **"any"** (#4479).
This document reads that as ***cross-project reach must be grantable***, not
*"defaults to all registered projects"*. The narrower reading is the one
consistent with default-deny (`C-3.2`), which is this epic's settled principle,
and with `C-4.6`'s explicit-grant posture. The wider reading would make the
default the exact exposure #4417 filed to close. See §14 **Q-C** — non-blocking;
this document proceeds on the narrower reading and says so rather than assuming
silently.

### 12.3 (c) — What is audit-logged on a cross-project read

**C-10.4** Every routed call whose target project differs from the delegator's
active project SHALL emit an audit record (§9) carrying **both** projects, the
sub-agent principal, and its delegator. A cross-project read is the interesting
forensic event, exactly as a denial is (`C-7.2`).

**C-10.5** `C-10.4` SHALL hold on the **allow** path, not only the deny path. A
permitted cross-project read that leaves no trace is the failure mode this clause
exists to prevent, and it is the shape that would survive a naive
"log the denials" implementation.

### 12.4 (d) — Which classes of action require interactive confirmation

**C-10.6** The following SHALL require interactive operator confirmation and
SHALL NOT be satisfiable by a persona configuration, a prior grant alone, or a
model-generated assertion:

1. **Any cross-project write.** A read may be granted standing; a write outside
   the active project is a distinct act.
2. **First use of a credential by a principal that has never resolved it before.**
   A first-use consent gate converts a mis-issued grant from a silent capability
   into a visible one, and is cheap: it fires once per `(Principal, CredentialRef)`.
3. **Any action outside the active project by a principal whose delegator
   ingested untrusted content in the same session.** This is the izzie/Gmail
   exfiltration shape #4417 names directly. It is grounded in machinery that
   partially exists — the delegation-taint concept is already real and test-pinned
   (`tainted_delegation_cannot_widen_into_the_l0_grant`,
   `crates/trusty-agents/src/runtime/tool_registry.rs`) — but the taint is not
   currently carried into a credential or project decision, so this is a **design
   requirement, not a description of current behaviour**.

**C-10.7** The confirmation gate specified here is about **credential and project
scope**. It composes with — and does **not** replace — the User-Authority
Singleton gate that governs computer-use screen/click/AppleScript actions (DOC-41
§5.5, #3074, #4439 Constraint 1), which is explicitly out of this document's scope
per #4040's own scheduling comment. An action may require both. Neither may be
read as satisfying the other.

**C-10.8** A confirmation SHALL be recorded in the audit stream with its outcome
(confirmed / declined / timed out). A gate whose decisions are not recorded cannot
be reviewed.

### 12.5 What this unblocks, precisely

**C-10.9** With `C-10.1`–`C-10.8` specified, #4479's floor change and #4417's
routing become implementable **without** recreating one hop out the exposure PR
#4420 closed. The three properties #4479 lists as *"unspecified"* — project
scoping, audit logging of cross-project reads, and a confirmation gate — are
`C-10.1`/`C-10.2`, `C-10.4`/`C-10.5`, and `C-10.6` respectively.

**C-10.10** Neither this section nor any other clause in this document adds
`version-control`, `computer-use`, or any other name to
`ASSISTANT_REACHABLE_SUBAGENTS`. That is a capability grant and it stays with
#4479 (and #4439, #4405), blocked where it is. This document says only what must
be true *before* such a change is safe.

---

## 13. SPEC-CREDAUTH-11 — The Seam with #4550 (Routed Shell Permissions) {#SPEC-CREDAUTH-11~draft}

**ID:** `SPEC-CREDAUTH-11~draft`
**Status:** Draft.

### 13.1 The split

**C-11.1** DOC-45 owns the **credential dimension**: which principal may resolve
which credential, at which scope, for which project. #4550 owns the **command
dimension**: which command a routed shell may run, under Claude Code's
allow/ask/deny semantics.

**C-11.2** The two SHALL compose as a logical **AND**. A routed action that needs
a credential proceeds only if the command gate allows it *and* the credential
authority grants it. Neither gate may grant what the other denies, and neither may
be implemented as an override of the other.

Concretely: an `l0_shell_exec`-style invocation of `gh pr list` requires (a)
#4550 to allow the command and (b) `C-3.2` to grant the principal the GitHub
credential at the requested scope. Today (a) does not exist and (b) is ambient
process environment.

### 13.2 The clause that makes #4550's gate meaningful

**C-11.3** A routed shell subprocess SHALL be spawned with a **scrubbed
environment**. Credential-bearing variables SHALL be removed by default, and only
those the invoking principal holds a grant for SHALL be re-injected, per `C-8.4`.

**C-11.4** Wholesale inheritance of the parent process environment by a routed
shell is **forbidden**. Without `C-11.3`/`C-11.4`, #4550's command gate is
decorative: a subprocess that inherits every credential the parent can see is one
`env` away from disclosing all of them, and no allow/ask/deny list over command
*text* can prevent that.

`crates/trusty-mpm/src/core/claude_env_scrub.rs::scrub_command` is existing prior
art for scrubbing a `Command`'s environment and is one of the eight redaction rule
sets #4569 consolidates; the scrubbing entry point here SHOULD be that
consolidated one rather than a ninth.

### 13.3 Both routes, one posture

**C-11.5** `C-11.1`–`C-11.4` SHALL apply identically to **both** sanctioned shell
routes — Computer Use (#3089) and the tcode PM (#4350) — per the owner's
2026-08-01 decision that both carry the *same* standard permissions. A
credential-dimension design written against one route only would let the two drift
into materially different security postures for what the decision treats as a
single capability reached two ways.

### 13.4 Audit

**C-11.6** Whether the permission decision and the credential decision share an
audit stream is **owner Q-A** (`C-7.9`). `C-7.10`/`C-7.11` bind both whichever way
it is answered.

---

## 14. Open Questions for the Owner

Two questions **block** parts of this document. One asks for confirmation of an
interpretation and blocks nothing. Nothing else is open — #4040's Q1 is answered
in §11 and its Q4 is answered in §6.

### Q-A (= #4040 Q2) — Audit granularity, and one stream or three? **BLOCKING**

**The fork.** (i) Is a credential audit record emitted **per resolution call**, or
**aggregated** per session/grant? (ii) Are the three audit streams —
credential access (#4567), shell execution (#4521), and permission allow/ask/deny
decisions (#4550) — **one stream with a discriminator** or **three independently
routable ones**?

**Why it is a genuine fork.** Per-call cannot lose an event and is the fail-safe
default, but a tight resolution loop makes a high-volume stream that an operator
will be tempted to suppress — and a suppressed audit stream is worse than an
aggregated one. On topology: one stream is simpler to route and retain and gives a
single ordered timeline of "what this agent did"; three are independently
suppressible without collateral, which matters because the three have genuinely
different sensitivity profiles (shell command text is the riskiest, credential refs
the least).

**What stays blocked.** `C-7.7`, `C-7.9`, and `C-7.12` are PROVISIONAL and MUST
NOT be implemented as written. #4567's acceptance criteria cannot be finalised.
#4521 and #4550 cannot settle their own record shapes without knowing whether they
share a stream. `C-7.1`–`C-7.6`, `C-7.8`, `C-7.10`, and `C-7.11` are settled
regardless and may proceed.

### Q-B (= #4040 Q3) — Do two assistant instances share a credential namespace? **BLOCKING**

**The fork.** Do `izzie` and `cto-assistant` — two instances with separate homes
per #4523 — resolve against **one** credential namespace or **two**?

**Why it is a genuine fork.** Per-instance is fail-closed and matches #4523's
per-instance homes: revoking `izzie`'s Gmail reach does not touch
`cto-assistant`'s. Per-type is materially less configuration: a user who adds a
second assistant of the same type does not re-authorise every provider, which is
the shape most likely to make people work around the system. This is the
granularity of `Principal` itself, so it cannot be deferred to implementation.

**What stays blocked.** `C-1.3` is PROVISIONAL; `Principal`'s `Assistant` variant
MUST NOT be implemented until this is answered. #4566's principal model is blocked
on it. Everything else in §3 — the four kinds, the session collapse, derivation
not self-declaration, the closed enumeration — is settled regardless.

### Q-C — Does "any registered project" mean *grantable* or *default*? **NON-BLOCKING**

**The fork.** Asked whether a routed sub-agent may reach any registered project or
only the active one, the owner answered **"any"** (#4479). Does that mean
cross-project reach is **grantable** (this document's reading, `C-10.3`), or that
a routed sub-agent's **default** scope is every registered project?

**Why it is asked.** The two readings produce opposite defaults, and the wider one
would make the default exactly the exposure #4417 filed to close. This document
proceeds on the narrower reading because it is the only one consistent with
default-deny, and surfaces the interpretation rather than burying it.

**What stays blocked: nothing.** `C-10.1`–`C-10.10` are implementable as written.
If the owner intends the wider reading, `C-10.2`'s default flips and nothing else
in this document changes.

---

## 15. How This Satisfies DOC-63 §7.1b

DOC-63 states exactly five requirements OKG Sources needs from #4040 *"and nothing
beyond them"*. #4563's acceptance criteria require that a reader of DOC-63 can
follow a pointer and find the answer. Each row below is that pointer.

| # | DOC-63 §7.1b requirement | Answered by | Notes |
|---|---|---|---|
| 1 | A durable, opaque reference a store row can hold in plain text under the home without that text being a secret | **`C-2.1`, `C-2.2`, `C-2.4`, `C-8.8`** | `C-2.2` supplies *durable* (stable across rotation); `C-2.4`'s restrictive grammar supplies *safe in plaintext* by construction, not by discipline. Satisfies `S-5.7`. |
| 2 | Resolution at use time to an authenticated client (or a token with a defined lifetime), performed outside the home and never materialised into it | **`C-3.4`, `C-8.4`, `C-8.6`, `C-8.7`** | `C-3.4` supplies the authenticated-client shape `S-5.1` requires; `C-8.7`'s construction-time path check is what makes "outside the home" structural rather than a rule. Satisfies `S-5.6`. |
| 3 | Read-scoped grants where the provider can issue them, and an honest machine-readable answer when it cannot | **`C-3.8`, `C-3.9`, `C-3.10`, `C-5.4`, `C-5.5`, `C-5.6`** | `C-5.5` `ScopeUnavailable` is the honest answer, kept distinct from `ZeroScope` by `C-5.6` precisely so the remediation is not "widen the grant" for a provider that has no such scope. Satisfies `S-5.3`, including its warning about the previously-made false read-only claim. |
| 4 | An observable revocation/expiry signal, so the displayed `needs-credential` state and schedule stop are driven by the authority rather than inferred from a 401 | **`C-6.1`, `C-6.2`, `C-6.3`, `C-6.5`** | `C-6.5`'s report channel is what makes this achievable rather than aspirational: the *first* 401 is a report, every subsequent consumer learns from the authority. Satisfies `S-5.4`. |
| 5 | Support for **both** OAuth and plain-API-key shapes — the API-key shape must not be special-cased | **`C-2.5`, `C-3.3`, `C-8.5`** | One entry point, one delivery path, no type or code path existing for only one shape. Granola (API key) and Gmail (OAuth) traverse identical machinery. |

**Also relevant to #4531 beyond the five:** `C-7.4` binds the run log and the
extraction manifest under `S-5.8`, and `C-8.1` makes DOC-63 `S-5.9`'s "no interim
token file" outcome the only expressible one.

---

## 16. What Each Blocked Consumer Gets

| Issue | What it asked for | Clause that unblocks it |
|---|---|---|
| **#4417** — route git through a Versioning sub-agent | *"The Versioning sub-agent must ENFORCE PROJECT SCOPING itself"*; what a routed call may reach | `C-10.1`, `C-10.2`, `C-10.4`, `C-10.6`; and `C-4.1`–`C-4.5` for the credential half — the sub-agent holds its own GitHub grant, the assistant lends nothing |
| **#4479** — add `version-control` to the reachable floor | The three properties it lists as *"unspecified"*: project scoping, cross-project audit, confirmation gate | `C-10.1`/`C-10.2`, `C-10.4`/`C-10.5`, `C-10.6` — see `C-10.9`. Note `C-10.10`: this document does **not** make the floor change |
| **#4439** — route shell through Computer Use, Constraint 2(b)(c)(d) only | (b) filesystem/project scope, (c) audit logging, (d) user confirmation | `C-10.1`/`C-10.2`, `C-10.4`/`C-10.5`, `C-10.6`. **Constraint 1 (authority-holder gating) is out of scope** — see `C-10.7` for how the two compose |
| **#4478** — tracker-agnostic ticketing, question (b) | *"how are per-provider credentials brokered"* | `C-2.7` (a ref resolves through the provider registry) + `C-3.1` (a grant is per-principal, so JIRA/Linear/GitHub selection is a grant question, not a hardcode). Question (b) is answerable once #4564 registers `JIRA_TOKEN`, `JIRA_API_TOKEN`, `LINEAR_API_KEY` — three of the 13 unmapped names |
| **#4550** — routed shell permissions on the Claude Code model | The credential dimension, and the seam so the two do not drift | §13 entire: `C-11.1`–`C-11.6`. `C-11.3`/`C-11.4` (scrubbed subprocess environment) is the clause without which #4550's command gate is decorative |
| **#4531** / DOC-63 — OKG Sources | The five requirements of §7.1b | §15's table, clause by clause |

---

## 17. Delivery Sequencing

This document changes no crate source. The children that implement it are already
filed and sequenced on #4040:

```
#4563 (THIS DOC + ADR-0026) ──┬──> #4566 (principal, ACL) ──> #4567 (audit)
                              │          ▲                        ▲
#4564 (provider registry) ────┴──> #4565 (CredentialRef) ──> #4568 (MCP refs)
                                                             
#4570 (at-rest storage) ◄── unblocked by §11
#4571 (consumer migration) ◄── required before C-3.2 is airtight (C-3.12)
```

**C-12.1** No child SHALL implement a PROVISIONAL clause before its owner question
is answered. #4566 is blocked on Q-B for `Principal`'s `Assistant` variant only;
#4567 is blocked on Q-A for cadence, topology, and retention only. Both may
proceed on everything else.

**C-12.2** #4570 is **unblocked** by §11. #4040's Q1 is answered; what remains in
#4570 is the narrower encryption / privileged-namespace question `C-9.6` names.

---

## 18. Revision History

| Date | Change |
|---|---|
| 2026-08-01 | Initial draft (#4563). Encodes four owner decisions: a grant does not survive delegation (§6, #4040 Q4); keyring default with loud plaintext fallback (§11, #4040 Q1); cross-product scope, trusty-agents **and** trusty-code (§2.1); `trusty-common` as the home, from the dependency graph (§1.3). Adds `ScopeUnavailable` as a fifth diagnostic distinct from `ZeroScope` (`C-5.6`) and flags it as an addition to #4040's stated four. Records that #4566's "delegation narrows" acceptance bullet describes the rejected inherit-narrowed model (`C-4.5`). Surfaces owner Q-A (audit granularity/topology, blocking #4567), Q-B (per-instance credential namespace, blocking #4566), and Q-C (non-blocking, the reading of "any registered project"). Corrects the spec catalog's note that `DOC-45` was claimed by the `spec-twin-lead-architecture` branch — that branch claims `DOC-44` only. |
