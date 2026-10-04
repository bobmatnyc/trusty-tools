# 0068. The deployable memory-store reference implementation lives in a separate repository; the conformance fake stays in trusty-tools

- **Status:** Proposed
- **Date:** 2026-10-04
- **Scope:** Workspace-wide — trusty-tools repository boundary; a future separate repository (not created)
- **Reversibility Cost:** Low until the repository is created; Medium after
- **Decision Drivers:** owner suggestion 2026-10-04 22:31Z (a completely separate repository holding a reference implementation, an option to evaluate); owner ruling 22:28Z (interchangeable backends, no required central store); ADR-0032; CI independence
- **Supersedes / Superseded by:** —

## Context

The API (ADR-0067) needs at least one deployable endpoint to prove it. Backend examples in the brainstorming note (DynamoDB on AWS, Vercel, Neon, Upstash) are examples, not selections. The closed vision issue [#1683](https://github.com/bobmatnyc/trusty-tools/issues/1683) proposed an in-repo server crate; it was closed as not planned on 2026-09-03.

ADR-0032 says no trusty-* service owns an HTTP daemon in this workspace; a hosted memory endpoint is an HTTP service by nature.

Options:

| Option | For | Against |
|---|---|---|
| A. Separate repository for the deployable endpoint; in-repo fake endpoint for conformance | Keeps cloud and deployment code out of the workspace; no conflict with ADR-0032; backends can vary per repository; trusty-tools CI never depends on it | Two repositories to keep in step; the conformance suite must be versioned and consumable |
| B. A server crate in trusty-tools | One repository, shared CI | Brings a deployable HTTP service and provider SDKs into the workspace; conflicts with ADR-0032's intent; ties backend choice to the workspace |
| C. No reference server; conformance suite only | Least code | Nothing proves the auth, revocation and deployment parts of the contract |

## Decision

We will take option A, deferred: a separate repository will hold the deployable reference endpoint, created only on Bob's GO at Stage 4 of DOC-80. Until then, trusty-tools carries an in-process fake endpoint and the conformance suite (DOC-80 §11), which are the only things its CI depends on.

## Consequences

- Stages 1 to 3 need no new repository and no cloud resources.
- The reference endpoint must pass the published conformance suite; it has no special status beyond that.
- Repository creation, provider choice and any GitHub App registration remain separate owner decisions.

## Related Decisions

Vetted against prior ADRs on 2026-10-04:

- **ADR-0032 (console is the only HTTP surface):** Consistent — the hosted endpoint lives outside the workspace.
- **ADR-0022 (separate per-store repositories for knowledge trees):** Consistent — same pattern of keeping store content and deployment outside the monorepo.
- **ADR-0067 (API spec location):** Extends — the contract stays in trusty-tools; only the deployable implementation leaves.
