# Authorship & Key-Person Risk: Technical Due Diligence

> Provenance: ⁽ᵐ⁾ measured (computed from the repository) · ⁽ᵈ⁾ declared (manifest / metrics input) · ⁽ⁱ⁾ inferred (LLM, evidence-grounded). Genuinely unknowable fields are omitted and listed under Gaps & Caveats.

Companion to the technical due-diligence report for Technical Due Diligence; generated 2026-08-23.

Development is heavily concentrated: five distinct authors over the trailing period but a bus factor of 1, with the top author accounting for 87% of touches and a long list of single-author subsystems (deploy, docker, python, tests, website, plus most configuration and policy files). The trailing 4-month trajectory is active and increasing — avg 740.8 commits/mo across avg 3.8 active authors/mo — which is a genuine positive signal of momentum, but the ownership concentration means continuity is materially exposed to the departure of one person. An acquirer should treat key-person risk as a primary integration concern and plan knowledge transfer across the single-author subsystems before it becomes a retention hostage. ⁽ⁱ⁾

| Application | Distinct authors | Bus factor | Top author share | Single-author subsystems | 12-mo trajectory |
|---|---|---|---|---|---|
| 00-local-trusty-tools | 5 ⁽ᵐ⁾ | 1 ⁽ᵐ⁾ | 87% ⁽ᵐ⁾ | .cargo, .doc-number-allowlist.tsv, .generation-artifact-allowlist.tsv, .sld-lint-allowlist.tsv, .trusty-tools, AGENTS.md, CHANGELOG.md, SECURITY.md, cliff.toml, clippy.toml, deny.toml, deploy, docker, python, tests, website ⁽ᵐ⁾ | increasing over the trailing 4 month(s): avg 740.8 commit(s)/mo across avg 3.8 active author(s)/mo ⁽ᵐ⁾ |

Derivation caveats: Squash-merge attribution: a GitHub squash-merge preserves the PR author, but a local `git merge --squash` by a human does not — this run cannot distinguish the two. Identity aliases are merged only as far as the collection pass resolved them: authors are grouped by `authors.canonical_email` where the resolver linked the commit, and by the raw commit email where it did not. No vendored-path exclusion: a checked-in vendor/dependency directory can make its committer look like the sole owner of thousands of paths. ⁽ᵐ⁾
