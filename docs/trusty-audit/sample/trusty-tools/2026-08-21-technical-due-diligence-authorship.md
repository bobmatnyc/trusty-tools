# Authorship & Key-Person Risk: Technical Due Diligence

> Provenance: ⁽ᵐ⁾ measured (computed from the repository) · ⁽ᵈ⁾ declared (manifest / metrics input) · ⁽ⁱ⁾ inferred (LLM, evidence-grounded). Genuinely unknowable fields are omitted and listed under Gaps & Caveats.

Companion to the technical due-diligence report for Technical Due Diligence; generated 2026-08-21.

Authorship signals concentrate risk sharply: across five distinct contributors the bus factor is 1, with the top author responsible for 87% of all touches. A long list of subsystems — including .cargo, deploy, docker, python, tests, and website configuration — are single-author, meaning operational, packaging, and test infrastructure all depend on one person's tacit knowledge. The trailing four-month trajectory shows increasing activity (avg 7213.5 commits/mo across avg 3.8 active authors/mo), which is a genuine positive on velocity, but the elevated commit volume against such a thin and concentrated author base amplifies rather than mitigates key-person exposure. An acquirer should treat continuity of the dominant author as a material integration dependency and plan for knowledge transfer before any transition. ⁽ⁱ⁾

| Application | Distinct authors | Bus factor | Top author share | Single-author subsystems | 12-mo trajectory |
|---|---|---|---|---|---|
| 00-local-trusty-tools | 5 ⁽ᵐ⁾ | 1 ⁽ᵐ⁾ | 87% ⁽ᵐ⁾ | .cargo, .doc-number-allowlist.tsv, .generation-artifact-allowlist.tsv, .sld-lint-allowlist.tsv, .trusty-tools, AGENTS.md, CHANGELOG.md, SECURITY.md, cliff.toml, clippy.toml, deny.toml, deploy, docker, python, tests, website ⁽ᵐ⁾ | increasing over the trailing 4 month(s): avg 7213.5 commit(s)/mo across avg 3.8 active author(s)/mo ⁽ᵐ⁾ |

Derivation caveats: Squash-merge attribution: a GitHub squash-merge preserves the PR author, but a local `git merge --squash` by a human does not — this run cannot distinguish the two. Identity aliases are merged only as far as the collection pass resolved them: authors are grouped by `authors.canonical_email` where the resolver linked the commit, and by the raw commit email where it did not. No vendored-path exclusion: a checked-in vendor/dependency directory can make its committer look like the sole owner of thousands of paths. ⁽ᵐ⁾
