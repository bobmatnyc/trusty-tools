# Technical Due-Diligence Analysis: Technical Due Diligence

> Provenance: ⁽ᵐ⁾ measured (computed from the repository) · ⁽ᵈ⁾ declared (manifest / metrics input) · ⁽ⁱ⁾ inferred (LLM, evidence-grounded). Genuinely unknowable fields are omitted and listed under Gaps & Caveats.

## 1. Report Metadata

| Field | Value |
|---|---|
| Source document | repository inspection (manifest: /Users/masa/dogfood-audit-rerun4/out/00-local-trusty-tools/manifest.toml) |
| Vendor / methodology | trusty-review report (repository inspection) v0.25.0 ⁽ᵐ⁾ |
| Report date / version | 2026-08-23 ⁽ᵐ⁾ |
| Inference models | openrouter — reviewer: anthropic/claude-opus-4.8; verifier: anthropic/claude-haiku-4.5; summarizer: anthropic/claude-haiku-4.5 (selected from the manifest's [inference] section) ⁽ᵐ⁾ |
| Target / deal codename | Technical Due Diligence |
| Applications / systems assessed | 00-local-trusty-tools ⁽ᵈ⁾ |
| Analysis generated | 2026-08-23 ⁽ᵐ⁾ |

## Key Facts

| Metric | Value |
|---|---|
| Codebase size (total LoC) | 1557669 ⁽ᵐ⁾ |
| File count | 6683 ⁽ᵐ⁾ |
| Primary languages | Rust, HTML, Shell, Svelte, TypeScript ⁽ᵐ⁾ |
| Complexity profile | A: simple (0-5): 58812 (89%); B: moderate (6-10): 4535 (7%); C: elevated (11-15): 1568 (2%); D: high (16-20): 638 (1%); F: very high (>20): 678 (1%) ⁽ᵐ⁾ |
| Number of authors | 5 ⁽ᵐ⁾ |
| Estimated work volume | Not computed — no upstream effort-estimation metric exists |
| 12-month trajectory | increasing over the trailing 4 month(s): avg 740.8 commit(s)/mo across avg 3.8 active author(s)/mo ⁽ᵐ⁾ |

## 2. Executive Summary

00-local-trusty-tools is a large (approx 1.56M LoC) Rust-centric workspace of 30 crates implementing a local developer/agent tooling platform: an MCP-based ticketing and tools server (trusty-common, trusty-mcp), a vector-search and embedding daemon (trusty-search, trusty-embedderd, using usearch/hnsw), memory and knowledge-base stores (trusty-memory, trusty-kb), an audit/grounding pipeline (trusty-audit), a multi-process daemon and control plane (trusty-mpm), and agent, review, TUI and web-UI components (trusty-agents, trusty-review, trusty-tui, Svelte UI). The workspace topology is clean and acyclic, anchored by shared cores trusty-common and trusty-mcp.

The risk profile is led by RED findings. The trusty-mpm control-plane session HTTP handlers (run/connect/stop/auth/list) have no authentication, and a claude_cmd override lets any local caller cause the daemon to execute an arbitrary binary in an arbitrary directory — reachable by any process on the host, a local privilege/execution risk. JQL queries in the Jira backend are built by unescaped interpolation of user input, permitting injection. In trusty-search, a documented persistent data-loss window in HNSW snapshot persistence and a usearch view() call that can SIGSEGV the whole process on a truncated snapshot are both stability/durability hazards. Amber-level concerns cluster around fail-open error handling, deliberately downgraded dependency-advisory gates in deny.toml, and a broad maintainability backlog. Coverage is thin — only 140 of 6683 files (2.1%) were examined, and batch 9 of 31 failed to parse, so unexamined code remains a material unknown. An acquirer must scope remediation of the unauthenticated control plane and injection paths before relying on this system. ⁽ⁱ⁾

**Contents**

- [1. Report Metadata](#1-report-metadata)
- [Key Facts](#key-facts)
- [3. Scoring Model Normalization](#3-scoring-model-normalization)
- [4. Per-Application Scorecard](#4-per-application-scorecard)
- [5. Findings by Severity](#5-findings-by-severity)
- [Code Quality & Architecture](#code-quality--architecture)
- [Security Posture](#security-posture)
- [Performance & Scalability](#performance--scalability)
- [6. Risk Registers](#6-risk-registers)
- [7. Graph-Ready Data Appendix](#7-graph-ready-data-appendix)
- [8. Ticketing & Delivery Traceability](#8-ticketing--delivery-traceability)
- [9. Gaps & Caveats](#9-gaps--caveats)
- [Synthesis Status](#synthesis-status)
- [Dependency Inventory](#dependency-inventory)
- [Investigation Coverage](#investigation-coverage)

### Top Risks

| # | Risk | Severity | Est. cost/effort | Affected application(s) |
|---|---|---|---|---|
| 1 | Control-plane session HTTP handlers have no authentication and a claude_cmd override permits arbitrary local binary execution, reachable by any process on the host. ⁽ⁱ⁾ | RED | High — add auth/authz layer to control plane ⁽ⁱ⁾ | 00-local-trusty-tools |
| 2 | JQL queries in the Jira backend are built via unescaped string interpolation of user input, permitting injection. ⁽ⁱ⁾ | RED | Moderate — parameterize/escape query inputs ⁽ⁱ⁾ | 00-local-trusty-tools |
| 3 | HNSW snapshot persistence has a documented persistent data-loss window and usearch view() can SIGSEGV the whole daemon on a truncated snapshot. ⁽ⁱ⁾ | RED | High — durability/crash-safety rework ⁽ⁱ⁾ | 00-local-trusty-tools |
| 4 | deny.toml downgrades unsound/yanked advisories and holds a non-self-expiring exception for RUSTSEC-2026-0258. ⁽ⁱ⁾ | AMBER | Moderate — tighten dependency gates ⁽ⁱ⁾ | 00-local-trusty-tools |
| 5 | Bus factor of 1 with 87% of touches by one author across many single-author subsystems creates continuity risk. ⁽ⁱ⁾ | AMBER | Ongoing — key-person mitigation ⁽ⁱ⁾ | 00-local-trusty-tools |

## 3. Scoring Model Normalization

Captures the SOURCE report's native scale so a reader can compare this
report against others normalized through this same template.

| Field | Value |
|---|---|
| Native scale | trusty-review normalized 0–100 quality scale (higher is healthier) ⁽ᵐ⁾ |
| Native band definitions | RED < 33 (critical); AMBER 33–66 (medium risk); GREEN > 66 (healthy) ⁽ᵐ⁾ |
| Normalized mapping (0-100) | already 0–100; RED/AMBER/GREEN bands applied directly ⁽ᵐ⁾ |
| RED threshold (normalized) | < 33 ⁽ᵐ⁾ |
| AMBER threshold (normalized) | 33–66 ⁽ᵐ⁾ |
| GREEN threshold (normalized) | > 66 ⁽ᵐ⁾ |
| Aggregation method (app-level from module/rule level) | severity bands from repository inspection; application posture aggregates its findings ⁽ᵐ⁾ |
| Peer-benchmark population (if any) | cross-repo corpus placement when --benchmark is enabled; otherwise not computed ⁽ᵐ⁾ |

## 4. Per-Application Scorecard

### 4.1. 00-local-trusty-tools

**Profile**

| Field | Value |
|---|---|
| Technology stack | Rust 1,412,769 · HTML 33,376 · Shell 31,635 · Svelte 28,099 ⁽ᵐ⁾ |
| Frameworks / manifests | Cargo.toml: ⁽ᵐ⁾ |
| Technical size (LoC) | 1557669 ⁽ᵐ⁾ |
| Files / classes / DB artifacts | 6683 files ⁽ᵐ⁾ |

**Health-Factor Scores**

_No data available — see Gaps & Caveats._

**Benchmark Position**

_No data available — see Gaps & Caveats._

## 5. Findings by Severity

### 5.1 RED / CRITICAL Findings (full detail)

**00-local-trusty-tools:**

1. **JQL queries built via unescaped string interpolation of user input** — User-supplied query, assignee, label, and priority values are interpolated directly into JQL strings with no escaping, permitting JQL injection ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/jira/backend.rs:150
- **Business impact:** A crafted query could break out of the intended filter to read or manipulate arbitrary JIRA issues, and unbalanced quotes cause query failures ⁽ⁱ⁾
- **Remediation:** Escape or parameterize all user-supplied values before embedding them into JQL ⁽ⁱ⁾ (cost/effort: medium ⁽ⁱ⁾)
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/jira/backend.rs:150`) ⁽ᵐ⁾:
```
if let Some(q) = &p.query {
            jql.push_str(&format!(" AND text ~ \"{q}\""));
        }
```
- **Trace:** verified-by-trace (`JiraBackend::search_issues` declared at crates/trusty-common/src/tickets/api/backends/jira/backend.rs:148, 4 usage site(s) in the cited file) — The trace shows the vulnerable code at line 150 uses unescaped string interpolation `format!(" AND text ~ \"{q}\"")` directly into the JQL query without any escaping function or sanitization applied to the user-supplied `q` parameter.

2. **Control-plane session HTTP handlers have no authentication or authorization** — The session run/connect/stop/auth/list endpoints accept requests without any authentication or caller identity check ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/api/control_routes.rs:259
- **Business impact:** Any process able to reach the daemon HTTP port can spawn sessions, stop others' work, or steal write locks, giving unauthenticated control-plane access ⁽ⁱ⁾
- **Remediation:** Add an auth middleware (token or local-socket peer check) in front of the `/api/v1/control/sessions` routes before exposing the daemon ⁽ⁱ⁾ (cost/effort: medium ⁽ⁱ⁾)
- **Evidence** (`crates/trusty-mpm/src/daemon/api/control_routes.rs:259`) ⁽ᵐ⁾:
```
pub async fn ctl_run_session(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<CtlRunRequest>,
) -> Result<Json<CtlRunResponse>, (StatusCode, String)> {
```
- **Trace:** verified-by-trace (`ctl_run_session` declared at crates/trusty-mpm/src/daemon/api/control_routes.rs:259, 5 usage site(s) in the cited file) — The function signature at line 259 shows no authentication middleware, guard, or identity extraction in the handler parameters, and the test router at line 497 registers the endpoint without any auth layer.

3. **claude_cmd override lets callers designate an arbitrary executable path** — An unauthenticated caller supplying `claude_cmd` and `workdir` can cause the daemon to execute an arbitrary binary in an arbitrary directory ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/api/control_routes.rs:267
- **Business impact:** not stated in source data
- **Remediation:** Validate/allow-list the executable path and require authentication before honouring caller-supplied command or workdir overrides ⁽ⁱ⁾ (cost/effort: medium ⁽ⁱ⁾)
- **Evidence** (`crates/trusty-mpm/src/daemon/api/control_routes.rs:267`) ⁽ᵐ⁾:
```
backend,
        prompt_file: req.prompt_file.map(Into::into),
        claude_cmd: req.claude_cmd,
    };
```
- **Trace:** verified-by-trace (`ctl_run_session` declared at crates/trusty-mpm/src/daemon/api/control_routes.rs:259, 5 usage site(s) in the cited file) — The trace shows `claude_cmd` is passed directly from the request into `RunParams` without any validation, sanitization, or authentication check visible in the `ctl_run_session` function.

4. **Documented persistent data-loss window in HNSW snapshot persistence (issue #3970)** — The shrink guard provides essentially no protection during any large reindex, so an ungraceful termination permanently strands the index at a partial fraction with no path back to the original complete snapshot ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/store/usearch_store.rs:169
- **Business impact:** An OOM-kill, SIGKILL, or power loss mid-reindex can permanently corrupt a customer's search index to a partial state, requiring a full re-index to recover ⁽ⁱ⁾
- **Remediation:** Implement the recommended staged-write-then-swap for the HNSW snapshot, mirroring the redb corpus approach from #603/#839 ⁽ⁱ⁾ (cost/effort: medium ⁽ⁱ⁾)
- **Evidence** (`crates/trusty-search/src/core/store/usearch_store.rs:169`) ⁽ᵐ⁾:
```
the complete pre-reindex on-disk
/// snapshot has already been overwritten by a partial, still-growing one,
/// during ordinary healthy operation. From then on an ungraceful termination
/// (SIGKILL, OOM-kill, process abort, power loss — none of which run
```

5. **usearch view() can SIGSEGV the entire process on a truncated snapshot file** — Calling usearch's view() on a small truncated or garbage snapshot triggers undefined C++ behavior that crashes the daemon rather than returning a recoverable error ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/store/usearch_store.rs:188
- **Business impact:** A single corrupted on-disk snapshot can crash the entire daemon serving hundreds of indexes, causing a full-service outage rather than degrading one index ⁽ⁱ⁾
- **Remediation:** Retain and thoroughly test the size-floor guard, and consider sandboxing the FFI view call or validating usearch's header more defensively ⁽ⁱ⁾ (cost/effort: medium ⁽ⁱ⁾)
- **Evidence** (`crates/trusty-search/src/core/store/usearch_store.rs:188`) ⁽ᵐ⁾:
```
does
/// **not** reliably return an `Err` — it can **SIGSEGV the whole process**.
```

### 5.2 AMBER / MEDIUM Findings (full detail, more compact)

**00-local-trusty-tools:**

1. **Split oversized impl block** — cyclomatic complexity 140 (grade F); smells: long_impl_block(603 lines), deep_nesting(depth 6)
- **Component:** crates/trusty-common/src/memory_core/store/hnsw_store.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 342–945) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

2. **Extract method — dispatch** — cyclomatic complexity 118 (grade F); smells: long_function(235 lines), deep_nesting(depth 5), missing_docstring
- **Component:** crates/trusty-common/src/tickets/server.rs
- **Business impact:** not stated in source data
- **Remediation:** Extract the body of 'dispatch' (lines 88–323) into 2–3 smaller functions

3. **Split oversized impl block** — cyclomatic complexity 112 (grade F); smells: long_impl_block(489 lines), deep_nesting(depth 5), missing_docstring
- **Component:** crates/trusty-common/src/tickets/api/backends/jira/backend.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 23–512) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

4. **Split oversized impl block** — cyclomatic complexity 98 (grade F); smells: long_impl_block(723 lines), deep_nesting(depth 6)
- **Component:** crates/trusty-git-analytics/src/collect/azdo/client.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 40–763) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

5. **Split oversized impl block** — cyclomatic complexity 94 (grade F); smells: long_impl_block(458 lines), deep_nesting(depth 6)
- **Component:** crates/trusty-common/src/memory_core/store/chat_sessions/store.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 44–502) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

6. **Extract method — run** — cyclomatic complexity 92 (grade F); smells: long_function(354 lines), deep_nesting(depth 7), missing_docstring
- **Component:** crates/trusty-search/src/main.rs
- **Business impact:** not stated in source data
- **Remediation:** Extract the body of 'run' (lines 1117–1471) into 2–3 smaller functions

7. **Split oversized impl block** — cyclomatic complexity 91 (grade F); smells: long_impl_block(396 lines), deep_nesting(depth 6)
- **Component:** crates/trusty-search/src/core/corpus/corpus_ops.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 20–416) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

8. **Extract method — classify** — cyclomatic complexity 90 (grade F); smells: long_function(133 lines), deep_nesting(depth 6), missing_docstring
- **Component:** crates/trusty-common/src/symgraph/symbol.rs
- **Business impact:** not stated in source data
- **Remediation:** Extract the body of 'classify' (lines 143–276) into 2–3 smaller functions

9. **Split oversized impl block** — cyclomatic complexity 89 (grade F); smells: long_impl_block(410 lines), deep_nesting(depth 6), missing_docstring
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 20–430) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

10. **Split oversized impl block** — cyclomatic complexity 89 (grade F); smells: long_impl_block(400 lines), deep_nesting(depth 6)
- **Component:** crates/trusty-common/src/memory_core/store/payload_store/store.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 42–442) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

11. **Split oversized impl block** — cyclomatic complexity 89 (grade F); smells: long_impl_block(418 lines), deep_nesting(depth 7)
- **Component:** crates/trusty-search/src/core/corpus/meta_ops.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 19–437) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

12. **Split oversized impl block** — cyclomatic complexity 88 (grade F); smells: long_impl_block(504 lines), deep_nesting(depth 6), missing_docstring
- **Component:** crates/trusty-common/src/tickets/api/backends/linear/backend.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 24–528) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

13. **Split oversized impl block** — cyclomatic complexity 86 (grade F); smells: long_impl_block(728 lines), deep_nesting(depth 5)
- **Component:** crates/trusty-audit/src/session.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 441–1169) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

14. **Split oversized impl block** — cyclomatic complexity 84 (grade F); smells: long_impl_block(1072 lines), deep_nesting(depth 7)
- **Component:** crates/trusty-mpm/src/session_manager/manager.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 223–1295) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

15. **Split oversized impl block** — cyclomatic complexity 80 (grade F); smells: long_impl_block(333 lines), deep_nesting(depth 5), missing_docstring
- **Component:** crates/trusty-agents/src/memory/redb_usearch/store_impl.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 20–353) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

16. **Split oversized impl block** — cyclomatic complexity 80 (grade F); smells: long_impl_block(875 lines), deep_nesting(depth 7)
- **Component:** crates/trusty-common/src/memory_core/retrieval/handle.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 159–1034) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

17. **Split oversized impl block** — cyclomatic complexity 79 (grade F); smells: long_impl_block(160 lines)
- **Component:** crates/trusty-agents/src/inspection/task_signals.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 35–195) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

18. **Extract method — TaskSignals::extract** — cyclomatic complexity 79 (grade F); smells: long_function(145 lines), missing_docstring
- **Component:** crates/trusty-agents/src/inspection/task_signals.rs
- **Business impact:** not stated in source data
- **Remediation:** Extract the body of 'TaskSignals::extract' (lines 49–194) into 2–3 smaller functions

19. **Extract method — SurfaceWalker** — cyclomatic complexity 79 (grade F); smells: long_function(224 lines), deep_nesting(depth 6)
- **Component:** scripts/lib/rustdoc_walk.py
- **Business impact:** not stated in source data
- **Remediation:** Extract the body of 'SurfaceWalker' (lines 193–417) into 2–3 smaller functions

20. **Split oversized impl block** — cyclomatic complexity 77 (grade F); smells: long_impl_block(728 lines), deep_nesting(depth 8)
- **Component:** crates/trusty-search/src/core/store/usearch_store.rs
- **Business impact:** not stated in source data
- **Remediation:** Split the impl block (lines 381–1109) across focused submodules — the analyzer reported no function name for this region, so it is a whole-impl hotspot rather than one long function

21. **hnsw_rs cannot remove points, leaving stale shadow vectors in the graph** — Deletes only write tombstones and re-upserts leave prior embeddings resident in the in-memory graph, requiring a shadowed-id set and per-open rebuild to stay correct ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/hnsw_store.rs:327
- **Business impact:** Stale graph state can surface incorrect search results and grows memory over a session's lifetime until the next open reclaims it ⁽ⁱ⁾
- **Remediation:** Track memory growth of orphaned shadow points and consider periodic in-process graph rebuild or a HNSW implementation that supports removal ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/hnsw_store.rs:327`) ⁽ᵐ⁾:
```
`hnsw_rs` cannot remove a point, so re-upserting a mapped
    /// uuid leaves its previous embedding in the graph under the same id. A
    /// search that scores the drawer by that stale copy reports content the
    /// drawer no longer holds. Knowing exactly which ids are ambiguous lets
```

22. **Multi-transaction compact_orphans creates a reachable inconsistent state** — Orphan compaction spans three separate transactions, so a concurrent upsert can have its VECTORS row deleted while its key survives, a state the code must defensively work around ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/hnsw_store.rs:221
- **Business impact:** Under in-process concurrency this window can silently drop a drawer's vector, causing unretrievable content that count-based health checks miss ⁽ⁱ⁾
- **Remediation:** Perform orphan detection and deletion within a single write transaction or otherwise serialize it against upserts ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/hnsw_store.rs:221`) ⁽ᵐ⁾:
```
deletes them in three separate
/// transactions, so an `upsert` that lands between the first and the third has
/// its brand-new id treated as an orphan and its `VECTORS` row removed while
/// the key survives. Under the in-process concurrency this store supports that
/// is a reachable state, and a `VECTORS`-only bound would hand the id straight
```

23. **Silent aliasing bug history indicates fragile state invariants** — Documentation records a prior aliasing defect where multiple drawers silently mapped to one vector id with no health signal detecting it ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/hnsw_store.rs:260
- **Business impact:** Silent data-integrity failures that evade all monitoring can degrade product correctness without detection until users report missing content ⁽ⁱ⁾
- **Remediation:** Ensure the AliasAudit runs proactively on open and surfaces alarms, and add regression tests covering concurrent id allocation paths ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/hnsw_store.rs:260`) ⁽ᵐ⁾:
```
every count-based health signal this palace had could
/// be zero while four drawers were unretrievable — `drawer_count` matched,
/// `palace_reembed` reported nothing missing, and recall could still hit
/// lexically. `key_rows` versus `distinct_vector_ids` is the one comparison
/// that cannot be fooled by it, because it never leaves `VECTOR_KEYS`.
```

24. **MCP dispatch function is a monolithic 118-cyclomatic switch** — The single `dispatch` function routes all 30+ tools in one giant match arm with cyclomatic complexity of 118, concentrating every backend path into one unmaintainable function ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/server.rs:88
- **Business impact:** High-complexity dispatch is a change-risk hotspot where each new tool raises the chance of an undetected regression across unrelated tools ⁽ⁱ⁾
- **Remediation:** Extract per-tool handlers into a routing table or trait-dispatch map so each tool is independently testable and modifiable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/server.rs:88`) ⁽ᵐ⁾:
```
async fn dispatch(state: &AppState, name: &str, args: Value) -> Result<Value> {
    match name {
```

25. **MCP tool dispatch has only indirect/integration test coverage** — The 118-complexity dispatch surface routing all issue/comment/label/milestone operations is documented as only indirectly tested, and the unit tests exercise only initialize, tools/list, unknown-method, and list_backends ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/server.rs:79
- **Business impact:** Backend-mutating tool paths (create_issue, close_issue, delete_comment) can silently break without a failing test catching it ⁽ⁱ⁾
- **Remediation:** Add unit tests for each tool arm using a stub Backend to assert param mapping and result serialization ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/server.rs:79`) ⁽ᵐ⁾:
```
/// Test: indirect — covered by integration testing.
pub async fn handle_tool_call(state: &AppState, name: &str, args: Value) -> Value {
```

26. **run_stdio and transition tools marked untested or manually tested only** — The stdio loop entry point that wires the whole server to real I/O relies on manual verification rather than an automated test ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/server.rs:360
- **Business impact:** Regressions in the stdio request/response translation layer would only surface at runtime in production usage ⁽ⁱ⁾
- **Remediation:** Add a test that drives run_stdio_loop's closure with a synthetic Request and asserts the Response shape ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/server.rs:360`) ⁽ᵐ⁾:
```
/// Test: manual via Claude Code.
pub async fn run_stdio(state: AppState) -> Result<()> {
```

27. **External search daemon errors are fail-open, silently degrading DD coverage** — Every query error is captured as a failure line and discovery continues, so a partially or fully dead search daemon yields a degraded ranking rather than an explicit hard stop ⁽ⁱ⁾
- **Component:** crates/trusty-audit/src/grounding/evidence.rs:487
- **Business impact:** Audit reports could be produced with silently incomplete evidence, undermining trust in the DD conclusions ⁽ⁱ⁾
- **Remediation:** Escalate a total-failure threshold (e.g. all queries failing) into a hard error rather than an always-empty fail-open result ⁽ⁱ⁾
- **Evidence** (`crates/trusty-audit/src/grounding/evidence.rs:487`) ⁽ᵐ⁾:
```
Ok(hits) => merge(&mut scored, &hits, query),
                Err(cause) => discovery.failures.push(cause),
```
- **Trace:** verified-by-trace (`discover` declared at crates/trusty-audit/src/grounding/evidence.rs:470, 5 usage site(s) in the cited file) — The `discover` function captures query errors into `discovery.failures` at line 487 and continues execution without returning early or raising an exception, confirming the fail-open behavior.

28. **Search response deserialization tolerates missing fields via serde defaults** — All response fields default silently, so a malformed or schema-changed daemon response yields zero-score/empty-path chunks that are filtered out rather than raising a version-mismatch error ⁽ⁱ⁾
- **Component:** crates/trusty-audit/src/grounding/evidence.rs:355
- **Business impact:** A silent daemon contract change would degrade evidence quality with no diagnostic, mimicking a working-but-empty result ⁽ⁱ⁾
- **Remediation:** Require the score/path fields explicitly or emit a diagnostic when defaults are applied on non-empty responses ⁽ⁱ⁾
- **Evidence** (`crates/trusty-audit/src/grounding/evidence.rs:355`) ⁽ᵐ⁾:
```
struct Chunk {
    #[serde(default)]
    file: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    score: f32,
```
- **Trace:** verified-by-trace (`Chunk` declared at crates/trusty-audit/src/grounding/evidence.rs:355, 5 usage site(s) in the cited file) — The `Chunk` struct at line 355 declares `#[serde(default)]` on all three fields (`file`, `path`, and `score`), which allows deserialization to succeed with missing fields by using their default values rather than raising an error.

29. **Time-boxed dependency vulnerability exception cannot self-expire** — A known advisory (RUSTSEC-2026-0258) is suppressed with a manual review date that cargo-deny cannot enforce, so the exception persists silently until a human deletes it ⁽ⁱ⁾
- **Component:** deny.toml:231
- **Business impact:** An unpatched h2 vulnerability can remain masked in the release gate indefinitely if the review date slips unnoticed ⁽ⁱ⁾
- **Remediation:** Track the review deadline in an external ticket with an alarm and complete the hyper 1.x migration to clear the advisory ⁽ⁱ⁾
- **Evidence** (`deny.toml:231`) ⁽ᵐ⁾:
```
ignore = [
  { id = "RUSTSEC-2026-0258", reason = "REVIEW BY 2026-09-18. h2 unbounded empty DATA frames (low severity).
```

30. **Yanked and unsound advisories deliberately downgraded from deny** — The gate disables cargo-deny's unsound and unmaintained checks and sets yanked to warn, accepting four unsound RustSec advisories (anyhow, git2, lru) as unactionable ⁽ⁱ⁾
- **Component:** deny.toml:211
- **Business impact:** Transitive unsound dependencies can ship in published artifacts without a hard gate, deferring risk to future upgrade work ⁽ⁱ⁾
- **Remediation:** Schedule dependency upgrades to clear the transitive advisories and re-enable stricter gating once patched versions are reachable ⁽ⁱ⁾
- **Evidence** (`deny.toml:211`) ⁽ᵐ⁾:
```
unsound = "none"
```

31. **Static ONNX Runtime linkage causes indefinite init deadlock on AL2023/glibc 2.34** — The bundled ONNX Runtime assumes glibc >= 2.38 while AL2023 ships 2.34, causing a hard deadlock that the timeout guard only mitigates by exiting rather than fixing ⁽ⁱ⁾
- **Component:** crates/trusty-embedderd/src/readiness.rs:4
- **Business impact:** Embedder deployments on common older-glibc hosts silently degrade semantic search to lexical-only until an operator intervenes ⁽ⁱ⁾
- **Remediation:** Ship a dynamically-linked ORT build or platform-specific release assets so init succeeds without the timeout fallback ⁽ⁱ⁾
- **Evidence** (`crates/trusty-embedderd/src/readiness.rs:4`) ⁽ᵐ⁾:
```
the ORT
//! CPU(no-arena) execution-provider init deadlocks in `futex_wait_queue`
//! *indefinitely* (observed 12+ consecutive spawns over several hours, 0% CPU,
```

32. **Concurrency limiter reads env var once and caches for process lifetime** — The queue timeout is cached in a process-global OnceLock on first call, so any later env change is ignored and the value is fixed for the daemon's life ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/concurrency.rs:65
- **Business impact:** Operators cannot retune the queue timeout at runtime and test/prod ordering can lock in an unintended value ⁽ⁱ⁾
- **Remediation:** Resolve the timeout at limiter construction from injected config rather than a global lazily-cached env read ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/concurrency.rs:65`) ⁽ᵐ⁾:
```
static CACHED: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
```

33. **Abandoned blocking thread on model-init timeout** — On timeout the blocked OS thread is intentionally leaked, relying on the process exiting immediately afterward to avoid accumulation ⁽ⁱ⁾
- **Component:** crates/trusty-embedderd/src/readiness.rs:29
- **Business impact:** If this pattern is reused in a long-lived caller, repeated retries would leak blocking-pool threads and exhaust resources ⁽ⁱ⁾
- **Remediation:** Document the process-exit invariant as an enforced contract or provide a cancellable init path for reusable callers ⁽ⁱ⁾
- **Evidence** (`crates/trusty-embedderd/src/readiness.rs:29`) ⁽ᵐ⁾:
```
the still-blocked OS thread inside `spawn_blocking`
//! (`FastEmbedder::with_cache_size` runs the ORT init on a blocking-pool
//! thread) is abandoned, exactly like the existing CoreML-hang bound in
//! `trusty_common::embedder::fast_embedder::try_new_bounded` (issue #2111).
```

34. **Env-mutating tests run under default cargo parallelism risking flakiness** — This test mutates process-global env vars without a lock while its own comment acknowledges it must not run concurrently with others touching the same vars ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/concurrency.rs:323
- **Business impact:** Flaky or falsely-passing concurrency tests undermine confidence in the CI gate and can mask real regressions ⁽ⁱ⁾
- **Remediation:** Guard env-mutating tests with a shared mutex as done in the embedderd readiness tests ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/concurrency.rs:323`) ⁽ᵐ⁾:
```
std::env::remove_var("TRUSTY_MAX_CONCURRENT_REQUESTS");
        std::env::remove_var("TRUSTY_QUEUE_DEPTH");
```

35. **Bearer token embedded in request headers with no verification of TLS transport** — The API token is read from the app store and attached to every request against http://localhost with no guarantee of an encrypted transport ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src/lib/transport.ts:49
- **Business impact:** A token sent over plaintext loopback HTTP could be sniffed by other local processes, and the empty-token fallback silently sends unauthenticated requests ⁽ⁱ⁾
- **Remediation:** Document and enforce the token lifecycle and consider rejecting requests when a token is expected but absent ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src/lib/transport.ts:49`) ⁽ᵐ⁾:
```
const t = getCurrentApiToken();
  return t ? { Authorization: `Bearer ${t}` } : {};
```

36. **OpenRouter credential silently defaults to empty string on resolution failure** — When the adapter supplies no credential the code falls back to an empty string via unwrap_or_default rather than surfacing the missing-key error early ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/llm/http/mod.rs:400
- **Business impact:** A missing key produces an authless request that fails downstream with an opaque provider error instead of a clear credential-not-found message ⁽ⁱ⁾
- **Remediation:** Propagate the resolution error rather than defaulting to an empty credential ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/llm/http/mod.rs:400`) ⁽ᵐ⁾:
```
trusty_common::credentials::resolve_key("openrouter").unwrap_or_default()
```

37. **list_issues honors offset but list_milestones/epics hardcode maxResults with no pagination** — Milestone, epic, and epic-issue queries cap results at 100 with no offset or paging loop, silently truncating larger result sets ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/jira/backend.rs:368
- **Business impact:** Projects with more than 100 epics or milestone issues will have data silently dropped, producing incorrect views ⁽ⁱ⁾
- **Remediation:** Add pagination loops that iterate startAt until all results are retrieved ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/jira/backend.rs:368`) ⁽ᵐ⁾:
```
let body = json!({ "jql": jql, "maxResults": 100 });
```

38. **Browser fallback caps every task at a hardcoded 10-minute deadline** — The polling loop rejects any task exceeding a fixed 10-minute wall-clock budget regardless of legitimate long-running workloads ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src/lib/transport.ts:166
- **Business impact:** Long-running agent tasks in browser/dev mode are abandoned as timeouts even when the backend is still making progress ⁽ⁱ⁾
- **Remediation:** Make the deadline configurable or align it with backend-communicated task limits ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src/lib/transport.ts:166`) ⁽ᵐ⁾:
```
const deadline = startMs + 10 * 60 * 1000;
```

39. **Azure DevOps PAT injected via HTTP Basic auth on every request without redaction** — The Personal Access Token is passed directly into HTTP Basic auth across many endpoints, and error branches return response text bodies that could echo sensitive content ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/azdo/client.rs:112
- **Business impact:** A leaked or mis-logged PAT grants broad access to the Azure DevOps organisation, and any accidental logging of request/response bodies risks credential exposure ⁽ⁱ⁾
- **Remediation:** Wrap the PAT in a redacting secret type, ensure no error path logs it, and confirm URLs/response bodies cannot contain the token ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/azdo/client.rs:112`) ⁽ᵐ⁾:
```
.basic_auth("", Some(&self.config.pat))
```

40. **Env-var-driven mutable global latch controls BM25 logging** — A process-wide static latch suppresses cap warnings across all index instances, meaning multiple concurrent indexes share one hidden logging state ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/bm25.rs:53
- **Business impact:** In a multi-tenant or multi-index deployment, one index hitting the cap silences the warning for all others, obscuring capacity problems ⁽ⁱ⁾
- **Remediation:** Move the logging latch into per-index state or rely on the reporting API for bulk callers as documented ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/bm25.rs:53`) ⁽ᵐ⁾:
```
static BM25_CAP_LOGGED: AtomicBool = AtomicBool::new(false);
```

41. **BM25 corpus cap read from unvalidated environment variable at runtime** — The corpus cap is resolved from an env var on every upsert, so a large override could allow unbounded memory growth that the default 50k cap is designed to prevent ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/bm25.rs:44
- **Business impact:** An operator setting an excessive cap value could exhaust memory on large repos, degrading or crashing the daemon ⁽ⁱ⁾
- **Remediation:** Add an upper bound or emit an explicit warning when the configured cap greatly exceeds the default ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/bm25.rs:44`) ⁽ᵐ⁾:
```
std::env::var("TRUSTY_BM25_CORPUS_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
```

42. **Blocking gh subprocess probe on PATCH request path** — Setting gh_user via PATCH spawns a blocking external `gh auth status` subprocess bounded only by a timeout, coupling request latency to an external CLI ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/managed_routes/project_registry_routes.rs:451
- **Business impact:** Slow or hung gh invocations can tie up blocking-pool threads under concurrent config edits, degrading daemon responsiveness ⁽ⁱ⁾
- **Remediation:** Cache or rate-limit gh auth probes and cap the number of concurrent blocking probes ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/managed_routes/project_registry_routes.rs:451`) ⁽ᵐ⁾:
```
tokio::task::spawn_blocking(|| probe_gh_auth(GH_DOCTOR_TIMEOUT))
                .await
                .unwrap_or_else(|e| {
```

43. **ADO project listing lacks pagination, silently capping at 100 results** — The projects endpoint fetches a single page of up to 100 with pagination deferred to a future phase, so large organisations lose projects beyond 100 ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/azdo/client.rs:158
- **Business impact:** Organisations with more than 100 projects will have data silently truncated, producing incomplete analytics ⁽ⁱ⁾
- **Remediation:** Implement continuation-token pagination for get_projects as noted in the doc comment ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/azdo/client.rs:158`) ⁽ᵐ⁾:
```
let url = format!("{}/_apis/projects?api-version=7.1&$top=100", self.org_url());
```

44. **OAuth refresh tokens stored on disk in plaintext JSON** — Live OAuth refresh tokens are persisted unencrypted as pretty-printed JSON, protected only by Unix 0600 file mode with no encryption at rest ⁽ⁱ⁾
- **Component:** crates/trusty-gworkspace/src/api/auth/storage/mod.rs:213
- **Business impact:** Any process running as the same user, a backup, or a filesystem leak exposes long-lived credentials granting access to Google Workspace accounts ⁽ⁱ⁾
- **Remediation:** Encrypt token contents at rest using an OS keychain/secret store rather than relying solely on file permissions ⁽ⁱ⁾
- **Evidence** (`crates/trusty-gworkspace/src/api/auth/storage/mod.rs:213`) ⁽ᵐ⁾:
```
let data = serde_json::to_string_pretty(tokens)?;
        std::fs::write(&target, data)
```

45. **Non-Unix platforms leave token file with no permission hardening** — On Windows the token file permission restriction is a silent no-op, relying on an unverified assumption that user-profile ACLs are adequate ⁽ⁱ⁾
- **Component:** crates/trusty-gworkspace/src/api/auth/storage/mod.rs:235
- **Business impact:** Refresh tokens on Windows deployments may be readable by unintended principals depending on the actual ACLs of the write location ⁽ⁱ⁾
- **Remediation:** Apply explicit Windows ACL restriction to the token file rather than a no-op ⁽ⁱ⁾
- **Evidence** (`crates/trusty-gworkspace/src/api/auth/storage/mod.rs:235`) ⁽ᵐ⁾:
```
#[cfg(not(unix))]
    fn restrict_permissions(_path: &std::path::Path) -> Result<()> {
        Ok(())
    }
```

46. **Git remote origin URL passed directly into shell clone command** — The origin URL, which may embed credentials for private repositories, is passed to a spawned git process and could surface in process listings or logs ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/managed_routes/inproject.rs:328
- **Business impact:** Credential-bearing clone URLs risk leaking into logs, ps output, or error messages during managed session provisioning ⁽ⁱ⁾
- **Remediation:** Sanitize/redact credentials from URLs in logs and prefer credential helpers over embedded credentials ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/managed_routes/inproject.rs:328`) ⁽ᵐ⁾:
```
.args(["clone", "--no-local", origin_url])
        .arg(base_path)
```

47. **Corrupt session history JSON silently discarded** — When a stored session's history JSON fails to parse it is silently replaced with an empty vector rather than surfacing an error, and this pattern is repeated across get/list/append ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/chat_sessions/store.rs:173
- **Business impact:** A single malformed record could silently wipe an entire user's chat history on read, and worse, a subsequent append would then persist the empty history ⁽ⁱ⁾
- **Remediation:** Propagate a decode error (or preserve the raw history) instead of defaulting to empty, so corruption is detected rather than data-destroying ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/chat_sessions/store.rs:173`) ⁽ᵐ⁾:
```
let history: Vec<ChatMessage> =
                serde_json::from_str(&record.history).unwrap_or_default();
```

48. **Two-tier project override can serve a stale but valid token** — The documented precedence still returns an expired project-level token over a valid user-level one, only emitting a throttled warning rather than falling back ⁽ⁱ⁾
- **Component:** crates/trusty-gworkspace/src/api/auth/storage/mod.rs:486
- **Business impact:** Operators can hit repeated 401 loops (issue #2946) despite holding valid credentials, degrading reliability of every MCP tool call ⁽ⁱ⁾
- **Remediation:** Consider preferring the valid user-level token when the project override is expired, or make the override contract configurable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-gworkspace/src/api/auth/storage/mod.rs:486`) ⁽ᵐ⁾:
```
if project.token.is_expired() && !user.token.is_expired() {
```

49. **Fixed 4-worker global index pool caps throughput process-wide** — Background indexing is bounded by a hardcoded process-wide pool of 4 workers and a 64-slot queue, silently dropping work under sustained bursts ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/index_dispatch.rs:62
- **Business impact:** Under heavy write workloads indexing silently falls behind and drops batches, causing search results to become stale without failing loudly ⁽ⁱ⁾
- **Remediation:** Make worker count and queue capacity configurable and expose the drop counters through operational monitoring ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/index_dispatch.rs:62`) ⁽ᵐ⁾:
```
pub(crate) const MAX_INDEX_WORKERS: usize = 4;
```

50. **API token resolved from environment variables without validation** — The desktop bridge presents whatever token is in the environment, intentionally not filtering empty values, so a misconfigured empty token is silently forwarded ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:68
- **Business impact:** An operator misconfiguration could weaken the auth guard on conversation-content streaming without any visible error at startup ⁽ⁱ⁾
- **Remediation:** Log a warning when the resolved token is empty and document the required env var configuration ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:68`) ⁽ᵐ⁾:
```
let token = std::env::var("TAGENT_API_TOKEN")
            .or_else(|_| std::env::var("OPEN_MPM_API_TOKEN"))
            .ok();
```

51. **Cross-process daemon start exclusion via flock is unsupported on non-Unix platforms** — The single-flight daemon-start guard hard-fails on any non-Unix platform, so Windows deployments cannot auto-start the daemon at all ⁽ⁱ⁾
- **Component:** crates/trusty-mcp/src/single_flight.rs:116
- **Business impact:** Windows users are blocked from the auto-start path and must run the daemon manually, limiting cross-platform scalability ⁽ⁱ⁾
- **Remediation:** Provide a Windows-compatible advisory-lock implementation (e.g. LockFileEx) or document the platform limitation prominently ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mcp/src/single_flight.rs:116`) ⁽ᵐ⁾:
```
return Err(anyhow!(
                "single-flight daemon start requires flock(2) and is not supported on this platform"
            ));
```

52. **Blocking flock acquisition can wedge a caller indefinitely with no timeout** — The exclusive lock acquisition blocks with no timeout, so if a winning starter hangs during its readiness wait every subsequent bridge blocks unboundedly ⁽ⁱ⁾
- **Component:** crates/trusty-mcp/src/single_flight.rs:98
- **Business impact:** A single stuck daemon-start holding the lock across its readiness wait can stall all concurrent bridges until the process dies ⁽ⁱ⁾
- **Remediation:** Add a bounded wait or watchdog on the lock acquisition so callers fail loudly rather than block forever ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mcp/src/single_flight.rs:98`) ⁽ᵐ⁾:
```
let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
```

53. **SSE token-stream bridge reconnects with fixed backoff and unbounded retries** — The bridge loops forever with a constant 2-second backoff and no exponential increase or circuit breaker on a permanently failing sidecar (e.g. persistent 401) ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:77
- **Business impact:** A permanently broken auth or endpoint state produces continuous log spam and retries with no escalation, masking the root cause ⁽ⁱ⁾
- **Remediation:** Introduce exponential backoff and distinguish permanent (401) from transient errors to stop retrying unrecoverable failures ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:77`) ⁽ᵐ⁾:
```
tracing::warn!(?e, "event bridge stream failed; retrying");
            }
            tokio::time::sleep(RECONNECT_BACKOFF).await;
```

54. **Per-file SymbolGraph rebuilt during graph expansion may be expensive at scale** — Graph expansion synchronously builds a SymbolGraph from each unique source file on the query read path, only cached for the duration of a single call ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/search/indexer/search.rs:293
- **Business impact:** For large files or many distinct top-K files, repeated per-query graph builds add unbounded latency to search under load ⁽ⁱ⁾
- **Remediation:** Cache SymbolGraphs across queries with invalidation on file changes rather than rebuilding per search call ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/search/indexer/search.rs:293`) ⁽ᵐ⁾:
```
.or_insert_with(|| SymbolGraph::build_from_file(&trigger.file).ok());
```

55. **JIRA credentials passed via environment variables with no rotation or secret-store integration** — JIRA API tokens and email are sourced from plain environment variables and Base64-encoded into a long-lived in-memory auth header ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/jira/client.rs:30
- **Business impact:** Environment-based secrets are easily leaked via process listings, logs, or crash dumps and lack rotation/audit controls ⁽ⁱ⁾
- **Remediation:** Integrate a secret manager and support token rotation instead of relying solely on environment variables ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/jira/client.rs:30`) ⁽ᵐ⁾:
```
let token = cfg
            .api_token
            .ok_or_else(|| anyhow!("jira: missing api_token (set JIRA_API_TOKEN)"))?;
```

56. **JIRA DELETE error path echoes raw server response body into error message** — The DELETE error path interpolates the full raw HTTP response body into the error, while GET/POST/PUT delegate to `ensure_ok`, giving inconsistent error handling ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/jira/client.rs:103
- **Business impact:** Raw upstream bodies may contain sensitive detail and inconsistent error surfaces make debugging and log-scrubbing harder ⁽ⁱ⁾
- **Remediation:** Route DELETE through the same `ensure_ok` classification as the other verbs and sanitize error bodies ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/jira/client.rs:103`) ⁽ᵐ⁾:
```
let text = resp.text().await.unwrap_or_default();
            bail!("jira DELETE failed: {status}: {text}");
```

57. **Version pinning mismatch between local launcher and its declared dependency** — A 0.1.3 launcher declares a path dependency pinned to trusty-agents 0.38.6, coupling releases to an exact version that will break the build if the workspace crate version drifts ⁽ⁱ⁾
- **Component:** crates/trusty-agents-local/Cargo.toml:10
- **Business impact:** Tight exact-version coupling across workspace crates raises the risk of version-conflict build breaks during routine version bumps ⁽ⁱ⁾
- **Remediation:** Use a version range or rely on the workspace path resolution rather than pinning an exact patch version ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents-local/Cargo.toml:10`) ⁽ᵐ⁾:
```
trusty-agents = { path = "../trusty-agents", version = "0.38.6" }
```

58. **Unbounded cross-project fan-out required an emergency concurrency cap** — Cross-project search previously issued unbounded concurrent per-index queries, overrunning the admission limiter and producing a 503 storm, mitigated only by a later concurrency cap flag ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/main.rs:507
- **Business impact:** Fan-out scalability limits mean large index counts can trigger self-inflicted overload if the cap is misconfigured or bypassed ⁽ⁱ⁾
- **Remediation:** Enforce the fan-out concurrency cap by default and add load tests validating behaviour at high index counts ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/main.rs:507`) ⁽ᵐ⁾:
```
An unbounded fan-out over ~150+ indexes issued every per-index
        /// query near-simultaneously and overran the daemon's admission
        /// limiter (503 `server_busy` storm). This caps in-flight per-index
        /// work so a large fan-out degrades gracefully instead of tripping the
```

59. **Fixed per-session command inbox capacity may block under load** — Each session actor uses a hard-coded command inbox capacity of 32, which can cause callers to block or back up if command throughput spikes ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/control/actor.rs:236
- **Business impact:** A non-configurable fixed queue depth can become a hidden bottleneck under high session command volume ⁽ⁱ⁾
- **Remediation:** Make the command inbox capacity configurable and document the backpressure semantics when full ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/control/actor.rs:236`) ⁽ᵐ⁾:
```
let (command_tx, command_rx) = mpsc::channel::<ActorCommand>(32);
```

60. **Explicit sensitive-path denylist bypass can index secret-adjacent temp roots** — An opt-in flag lets callers bypass the OS-temp-dir/app-support denylist, widening the surface for accidentally indexing sensitive scratch directories ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/allowlist/mod.rs:518
- **Business impact:** A misconfigured or over-eager caller could index ephemeral directories containing transient credentials or private data ⁽ⁱ⁾
- **Remediation:** Log and audit every use of the sensitive-path bypass and require an explicit operator-facing confirmation ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/allowlist/mod.rs:518`) ⁽ᵐ⁾:
```
an EXPLICIT, single-root index request that
/// opts in via `allow_sensitive_path: true`.
```

61. **Timed-out corpus opener is never aborted, permanently consuming a blocking-pool thread** — On a genuinely wedged volume the abandoned spawn_blocking thread may run indefinitely, leaking a thread from the finite blocking pool per stuck path ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/corpus/open_guard.rs:382
- **Business impact:** Repeated wedged opens on stuck volumes could exhaust the blocking-thread pool and degrade the daemon over time ⁽ⁱ⁾
- **Remediation:** Bound the number of concurrent leaked openers or surface a health signal that alerts operators to persistently wedged paths ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/corpus/open_guard.rs:382`) ⁽ᵐ⁾:
```
// The spawned attempt is NOT aborted — it keeps holding the
            // per-path mutex for as long as it actually runs (possibly a
            // long time, e.g. a TCC-denied volume, issue #718). Mark the
            // path wedged now so every later caller fails fast instead of
```

62. **Env-var-driven workspace layout resolution mutates process-global environment in tests unsafely** — Configuration resolution relies on process-global environment variables serialized only by a test-local mutex, which is fragile under any parallel access outside tests ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/workspace_layout.rs:382
- **Business impact:** Concurrent readers of env-driven config could observe inconsistent workspace roots leading to indexing or attribution errors ⁽ⁱ⁾
- **Remediation:** Prefer passing resolved config explicitly rather than reading mutable process env at resolution time ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/workspace_layout.rs:382`) ⁽ᵐ⁾:
```
unsafe {
            std::env::remove_var(WORKSPACE_ROOT_ENV);
            std::env::remove_var(WORKTREES_DIRNAME_ENV);
        }
```

63. **Memory leak via Box::leak on every custom filter config** — Any non-default FilterConfig leaks its compiled regex set permanently, unbounded by config churn ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/filter.rs:266
- **Business impact:** Long-running daemons that create custom filter configs will slowly accumulate leaked memory ⁽ⁱ⁾
- **Remediation:** Cache compiled patterns in a per-instance OnceLock owned by the struct rather than leaking ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/filter.rs:266`) ⁽ᵐ⁾:
```
Box::leak(compiled.into_boxed_slice())
```

64. **Session registry is in-memory only with no persistence** — All session state, transcripts, and usage totals live solely in a process HashMap with no durable backing ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/session/registry.rs:271
- **Business impact:** Daemon restart or crash loses every active session and its accumulated context, blocking horizontal scaling ⁽ⁱ⁾
- **Remediation:** Add a pluggable persistence backend behind the registry as the module docs already anticipate ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/session/registry.rs:271`) ⁽ᵐ⁾:
```
sessions: Mutex<HashMap<String, SessionEntry>>,
    ring_capacity: usize,
```

65. **Single global Mutex around all session state limits scalability** — Every session read/write serializes on one coarse Mutex over the entire session map ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/session/registry.rs:271
- **Business impact:** Under many concurrent sessions the lock becomes a throughput bottleneck for the daemon ⁽ⁱ⁾
- **Remediation:** Consider sharding the map (e.g. per-session locks or DashMap) to reduce contention ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/session/registry.rs:271`) ⁽ᵐ⁾:
```
sessions: Mutex<HashMap<String, SessionEntry>>,
```

66. **Secret detector cannot catch credentials in connection-string URLs** — The documented backtick-split limitation lets user-chosen passwords inside URLs bypass the secret gate ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/filter.rs:444
- **Business impact:** Credentials embedded in connection strings can be silently persisted to palace storage ⁽ⁱ⁾
- **Remediation:** Add explicit URL-credential detection for the `scheme://user:pass@host` shape ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/filter.rs:444`) ⁽ᵐ⁾:
```
`ss@host` splits into two fragments and is missed. The adversarial delta is
/// nil — the detector has always split on whitespace, so deliberate evasion
/// costs a space either way — but this is an accidental-storage hygiene gate,
```

67. **Git-SHA allowlist admits arbitrary 7-64 char numeric IDs** — Pure-digit tokens like account numbers in the SHA length band are allowlisted past the noise and secret heuristics ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/filter.rs:371
- **Business impact:** Sensitive numeric identifiers can slip through the filter and be stored unredacted ⁽ⁱ⁾
- **Remediation:** Require a mix of hex letters or a git-context signal before allowlisting a token as a SHA ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/filter.rs:371`) ⁽ᵐ⁾:
```
(GIT_SHA_MIN_LEN..=GIT_SHA_MAX_LEN).contains(&len)
        && token.bytes().all(|b| b.is_ascii_hexdigit())
```

68. **High-complexity classify function is hard to maintain and test** — The classify function spans nine language branches with cyclomatic complexity 90 but tests only cover rust, python, and go paths ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/symgraph/symbol.rs:143
- **Business impact:** Java, typescript, c, cpp, and javascript symbol extraction can silently regress without test coverage ⁽ⁱ⁾
- **Remediation:** Add per-language extraction tests for every branch and consider decomposing per-language mappers ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/symgraph/symbol.rs:143`) ⁽ᵐ⁾:
```
fn classify(node: Node, source: &str, lang: &str) -> Option<(SymbolKind, String)> {
```

69. **transition() silently swallows label-hint failures** — A failing label update after a state transition is discarded so the ticket's status label can silently diverge from its intended workflow state ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs:426
- **Business impact:** Tickets can report a state (e.g. in-progress) without the visible label, misleading operators and automation relying on the label convention ⁽ⁱ⁾
- **Remediation:** Log or surface the failed best-effort tagging rather than discarding it entirely with `let _ =` ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/github/client_impl.rs:426`) ⁽ᵐ⁾:
```
let _ = self.add_tags(id, &[l.to_string()]).await; // best-effort
        }
```

70. **Full JSON error bodies interpolated into bail! messages may leak API payloads** — Error paths embed the entire GitHub response value into propagated error strings across every method, which can surface sensitive or verbose payload data in logs ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs:49
- **Business impact:** Verbose interpolated API responses in error chains risk leaking repository/user detail into logs and complicate error triage ⁽ⁱ⁾
- **Remediation:** Extract a bounded, sanitized error summary from the response instead of interpolating the whole `Value` ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/github/client_impl.rs:49`) ⁽ᵐ⁾:
```
anyhow::bail!("GitHub create_ticket HTTP {status}: {v}");
```

71. **remove_tags issues sequential per-label DELETE requests with no batching or concurrency** — Removing many tags fires one blocking HTTP round-trip per label serially, scaling linearly with tag count ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs:249
- **Business impact:** Large tag sets or high-throughput ticket automation incur cumulative latency and GitHub rate-limit pressure ⁽ⁱ⁾
- **Remediation:** Bound concurrency (e.g. buffered join) or document/limit expected tag-count scale ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/github/client_impl.rs:249`) ⁽ᵐ⁾:
```
// GitHub requires DELETE per-label; loop sequentially.
        for t in tags {
```

72. **update_ticket performs an extra get_ticket round-trip to resolve label deltas** — Applying add/remove label deltas triggers a second network fetch of the current labels before the PATCH ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs:94
- **Business impact:** Doubles API calls for label edits, adding latency and rate-limit consumption under bulk updates ⁽ⁱ⁾
- **Remediation:** Cache or accept caller-provided current labels to avoid the extra fetch where possible ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/github/client_impl.rs:94`) ⁽ᵐ⁾:
```
let mut current: Vec<String> = if let Some(ls) = req.labels {
                ls
            } else {
                self.get_ticket(id).await?.labels
            };
```

73. **Live REST CRUD operations declared out of test scope** — The entire GitHub REST integration (the bulk of the adapter and a flagged 89-cyclomatic hotspot) has no automated coverage beyond construction ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/github/client_impl.rs:8
- **Business impact:** Regressions in request building, status handling, or response parsing can ship undetected since no mock/integration tests exercise these paths ⁽ⁱ⁾
- **Remediation:** Add mock-HTTP tests (e.g. a wiremock server) covering success and error branches of each method ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/github/client_impl.rs:8`) ⁽ᵐ⁾:
```
//! Test: Construction is covered in `ticketing::tests`; live REST calls are
//! env-dependent and out of scope.
```

74. **Rehydrate/eviction race required three review rounds and hand-rolled mutex synchronization** — The indexer's evict/rehydrate concurrency correctness depends on subtle manually-reasoned mutex-guarded generation counters that repeatedly regressed across review rounds ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/indexer/mod.rs:249
- **Business impact:** Fragile hand-tuned concurrency invariants risk silent stale/empty-corpus reads under memory pressure that are hard to reproduce and audit ⁽ⁱ⁾
- **Remediation:** Encapsulate the evict/rehydrate state machine in one type with exhaustive concurrency tests rather than distributed atomics plus a mutex ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/indexer/mod.rs:249`) ⁽ᵐ⁾:
```
Round-3 finding (HIGH): an `AtomicU64` only made the counter ITSELF
    /// atomic — it did nothing to stop a concurrent evict's bump-generation-
```

75. **Silent-empty-result failure modes acknowledged as recurring in search index state** — Multiple documented incidents describe corpus/HNSW failures surfacing as empty HTTP-200 results indistinguishable from a genuinely empty index ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/indexer/mod.rs:308
- **Business impact:** Users can receive silently wrong (empty) search results while health checks report ready, eroding trust and hiding data-loss-adjacent faults ⁽ⁱ⁾
- **Remediation:** Continue converging on explicit fault signaling and add end-to-end tests asserting fault responses instead of empty 200s ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/indexer/mod.rs:308`) ⁽ᵐ⁾:
```
so a search over an index holding 85,269
    /// chunks returned `results: []` at HTTP 200 for as long as the daemon ran.
```

76. **Secret key held as plaintext String with expose accessor** — API keys and board tokens are stored in memory as a plain heap String with no zeroize-on-drop, so plaintext credentials linger in memory after use ⁽ⁱ⁾
- **Component:** crates/trusty-audit/src/config.rs:44
- **Business impact:** A memory dump or core file could expose per-engagement OpenRouter and client JIRA/Linear credentials that the type system otherwise protects ⁽ⁱ⁾
- **Remediation:** Wrap the inner String in a zeroizing container (e.g. the zeroize crate's Zeroizing) so the plaintext is wiped when SecretKey drops ⁽ⁱ⁾
- **Evidence** (`crates/trusty-audit/src/config.rs:44`) ⁽ᵐ⁾:
```
pub struct SecretKey(String);
```

77. **OpenRouter API key resolved and held as plain String in daemon state** — The daemon's shared AppState carries the OpenRouter API key as a raw String with no redaction wrapper, risking accidental logging or Debug exposure ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/state.rs:183
- **Business impact:** An accidental `{:?}` render or log of AppState could leak the OpenRouter key, unlike the audit crate which uses a redacting SecretKey newtype ⁽ⁱ⁾
- **Remediation:** Store the key in a redacting/zeroizing newtype rather than a bare String in shared application state ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/state.rs:183`) ⁽ᵐ⁾:
```
pub openrouter_api_key: String,
```

78. **Environment-based OpenRouter enablement without validation of key presence** — The chat panel is toggled purely on whether OPENROUTER_API_KEY is set at startup, coupling secret configuration to environment variables read at process start ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/state.rs:170
- **Business impact:** Rotating or unsetting the key requires a daemon restart, and an empty/whitespace key may still flip the flag misleadingly ⁽ⁱ⁾
- **Remediation:** Validate the key is non-empty and provide a runtime reload path rather than a one-shot startup boolean ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/state.rs:170`) ⁽ᵐ⁾:
```
pub openrouter_enabled: bool,
```

79. **Broadcast channel drops events for lagging SSE subscribers** — The SSE fan-out uses a fixed 128-slot broadcast buffer, so a slow dashboard subscriber loses events under burst load with only a lag frame ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/state.rs:191
- **Business impact:** Under many concurrent dashboards or high event rates, UI state can silently desync, undermining operator monitoring at scale ⁽ⁱ⁾
- **Remediation:** Make the broadcast capacity configurable and document the resync path for lagged subscribers ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/state.rs:191`) ⁽ᵐ⁾:
```
cap of 128 buffers transient slow readers;
    /// if a receiver lags it gets `RecvError::Lagged` and we emit a `lag` frame.
```

80. **Best-effort filesystem scans silently swallow read errors** — Directory read failures (permission denied, corrupt path) are silently discarded and degrade to an empty/partial roster with no diagnostic surfaced ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/fs_browse/roster.rs:231
- **Business impact:** An operator on a machine with permission problems gets an empty picker with no error, making the failure hard to diagnose ⁽ⁱ⁾
- **Remediation:** Log skipped-directory errors at debug/warn level so silent degradation is at least observable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/fs_browse/roster.rs:231`) ⁽ᵐ⁾:
```
let Ok(owners) = std::fs::read_dir(mpm_root) else {
        return out;
    };
```

81. **Unbounded per-index DashMaps grow with index count** — Several per-index maps (reindex_progress, last_reindex_aborted_at, last-queried cache) grow with each registered index and have no visible eviction on index removal in this file ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/state.rs:109
- **Business impact:** On a daemon managing hundreds of indexes over time, stale per-index entries accumulate and consume memory without bound ⁽ⁱ⁾
- **Remediation:** Ensure DELETE /indexes/:id removes the corresponding entries from every per-index map ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/state.rs:109`) ⁽ᵐ⁾:
```
pub last_reindex_aborted_at: Arc<DashMap<IndexId, std::time::Instant>>,
```

82. **Credential authority resolves values without any authorization check** — The single credential resolution entry point accepts a principal and scope but performs no grant/ACL enforcement, so any caller reaching the function obtains the secret ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/credentials/authority.rs:39
- **Business impact:** Any code path with access to resolve() can obtain any configured credential, leaving default-deny access control unenforced until the deferred #4566 work lands ⁽ⁱ⁾
- **Remediation:** Complete the #4566 grant-check work so scope/principal are actually enforced inside resolve_with before the secret is materialised ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/credentials/authority.rs:39`) ⁽ᵐ⁾:
```
**There is no authorization here.** [`resolve`] takes a `Principal` and
//! checks no grant against it, because grants, the ACL, default-deny, and the
//! code-owned floor are #4566 (DOC-45 §5) and #4565 is explicitly scoped to the
```

83. **55 raw std::env::var credential reads bypass the credential authority** — The module docs concede that 55 credential-bearing env reads still bypass the single-entry-point resolution and its future guardrails ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/credentials/authority.rs:45
- **Business impact:** Secrets are still fetched through ad-hoc environment reads that skip registry validation and the planned grant check, undermining the centralised secret model ⁽ⁱ⁾
- **Remediation:** Migrate the remaining raw std::env::var credential reads through the credential authority as tracked in #4571 ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/credentials/authority.rs:45`) ⁽ᵐ⁾:
```
it is not airtight until #4571 migrates the 55
//! raw `std::env::var` reads, and this document does not claim it is.
```

84. **load_all reads the entire payload table into memory** — load_all with no filter iterates and materialises every row of the PAYLOADS table into a single Vec with no pagination or streaming ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/payload_store/store.rs:335
- **Business impact:** Startup rebuilds and unfiltered loads scale linearly with total payload count and can exhaust memory on large deployments ⁽ⁱ⁾
- **Remediation:** Expose a streaming/paginated iterator instead of collecting all rows into a Vec for unbounded loads ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/payload_store/store.rs:335`) ⁽ᵐ⁾:
```
let mut out = Vec::new();
        let iter = if let Some(seg) = segment_filter {
```

85. **iter_segment collects a whole segment into an owned Vec assuming bounded size** — The segment scan copies every value's raw bytes into an owned Vec on the assumption that per-segment row counts stay small ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/payload_store/store.rs:415
- **Business impact:** lookup_id_for_uuid and list_segment allocate the entire segment in memory per call, so a mis-sized segment degrades both latency and memory as it grows ⁽ⁱ⁾
- **Remediation:** Thread the redb transaction lifetime through a lazy iterator or add explicit size limits to avoid full-segment materialisation ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/payload_store/store.rs:415`) ⁽ᵐ⁾:
```
// is small (per-segment row counts are bounded by application sizing)
        // so this is acceptable.
        let mut rows: Vec<Result<RowBytes>> = Vec::new();
```

86. **Marker file resolution reads a process-global env var** — Marker file discovery depends on a mutable process-global environment variable, forcing the tests to serialise via a module-local mutex ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/ai_marker_config.rs:280
- **Business impact:** Reliance on global mutable env state makes concurrent or embedded use fragile and hard to reason about in multi-threaded runs ⁽ⁱ⁾
- **Remediation:** Allow the marker path to be passed explicitly through configuration rather than relying solely on a process-wide env var ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/ai_marker_config.rs:280`) ⁽ᵐ⁾:
```
let raw = match std::env::var(ENV_AI_MARKERS) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_MARKER_FILE.to_string(),
    };
```

87. **High-cyclomatic payload store CRUD hotspot** — The store module was flagged as a cyclomatic-89 hotspot with heavily repeated transaction/table/error mapping boilerplate across every CRUD method ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/store/payload_store/store.rs:37
- **Business impact:** The dense, repetitive error-mapping surface raises maintenance cost and the risk of divergent handling as methods evolve ⁽ⁱ⁾
- **Remediation:** Extract shared transaction/table-open helpers to reduce duplicated error mapping and lower the function complexity ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/store/payload_store/store.rs:37`) ⁽ᵐ⁾:
```
pub struct PayloadStore {
    pub(super) db: Arc<Database>,
    pub(super) path: PathBuf,
}
```

88. **copy_all_from omits derived KG tables but relies on external regeneration path not visible in this batch** — The bulk-copy deliberately excludes KG tables whose regeneration and contribution-carry depends on separate code paths (referenced but not present here), a fragile split that has already caused silent data loss per the PR #5527 note ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/corpus/meta_ops.rs:329
- **Business impact:** A future refactor that breaks the out-of-band contribution-copy could silently discard external graph contributions while reporting reindex success ⁽ⁱ⁾
- **Remediation:** Add an integration test asserting kg_contrib survives a reindex swap and centralize the copied-table contract ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/corpus/meta_ops.rs:329`) ⁽ᵐ⁾:
```
Tables copied: `CHUNKS_TABLE`, `ENTITIES_TABLE`, `FILE_HASHES_TABLE`,
    /// and `_meta` (indexed_root, schema_version).
```

89. **Broadcast event bus is process-global via OnceLock, coupling to a single-process deployment** — The event bus is a single in-process broadcast channel, so SSE fan-out cannot scale horizontally across multiple daemon instances without external message routing ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/events.rs:8
- **Business impact:** Multi-instance/HA deployment of the daemon would fragment event streams so browsers only see events emitted by the instance they connect to ⁽ⁱ⁾
- **Remediation:** Introduce an external pub/sub (e.g. Redis/NATS) fan-out layer if multi-instance scaling is required ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/events.rs:8`) ⁽ᵐ⁾:
```
A `tokio::sync::broadcast` channel exposed via a process-global `OnceLock`
//! lets any code path emit events without threading the bus through dozens of
//! function signatures, while SSE subscribers fan out to all browsers.
```

90. **Fixed 1024-slot broadcast channel silently drops events for slow subscribers** — A hard-coded broadcast capacity means high event volume or a lagging SSE subscriber causes tokio broadcast to drop the oldest events, losing UI telemetry under load ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/events.rs:70
- **Business impact:** Under bursty agent activity or slow browser clients, users lose real-time events with no recovery path, degrading the product's core telemetry surface ⁽ⁱ⁾
- **Remediation:** Make capacity configurable and add lag/drop metrics or a replay buffer for reconnecting subscribers ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/events.rs:70`) ⁽ᵐ⁾:
```
const CHANNEL_CAPACITY: usize = 1024;
```

91. **Env-var reads mutate process-global env with unsafe blocks in tests requiring serialization** — Configuration is sourced from process-global environment variables mutated via unsafe blocks and gated on serial_test to avoid cross-test interference, indicating shared mutable global state as the config mechanism ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/lazy_loader/mod.rs:99
- **Business impact:** Global env-var config makes runtime reconfiguration and per-request/per-tenant overrides impossible and increases risk of subtle test-ordering flakiness ⁽ⁱ⁾
- **Remediation:** Read env vars once into an injected config struct rather than reading the process environment ad hoc ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/lazy_loader/mod.rs:99`) ⁽ᵐ⁾:
```
unsafe { std::env::set_var("TRUSTY_WARMBOOT_MAX_INDEXES", "10") };
```

92. **Per-PR commit enrichment adds N+1 round-trips to PR collection** — Each pull request triggers at least one extra sequential paginated API call for its commits, so collection cost grows linearly with PR count and can be slow or rate-limited on large repos ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/bitbucket/client.rs:263
- **Business impact:** Full-history collection on large repositories may be slow and consume Bitbucket rate limits, delaying analytics refresh ⁽ⁱ⁾
- **Remediation:** Parallelize per-PR commit fetches with a bounded concurrency limit or batch where the API permits ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/bitbucket/client.rs:263`) ⁽ᵐ⁾:
```
match self.fetch_pr_commits(pr_number).await {
                    Ok(shas) => {
                        mapped.commit_shas = encode_commit_shas(&shas)?;
```

93. **Malformed timestamps silently fall back to current time** — A PR with an unparseable created_on timestamp is silently assigned the current time rather than being flagged, corrupting historical timing analytics without any warning ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/bitbucket/client.rs:441
- **Business impact:** Downstream time-series metrics can be silently skewed by injected wall-clock timestamps, undermining trust in analytics ⁽ⁱ⁾
- **Remediation:** Log a warning on parse failure and consider skipping or marking rows with invalid timestamps ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/bitbucket/client.rs:441`) ⁽ᵐ⁾:
```
let created_at = parse_ts(&pr.created_on).unwrap_or_else(Utc::now);
```

94. **Two divergent supervisor-config construction paths risk lost batch forwarding** — The into_common_for_tests path silently produces None batch size while production do_spawn sets it separately, an intentional divergence guarded only by comments and naming conventions ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/embedder_supervisor/mod.rs:157
- **Business impact:** A future call site using the wrong constructor would silently lose batch forwarding and degrade embedding throughput without any compile-time protection ⁽ⁱ⁾
- **Remediation:** Enforce the split via a type-level distinction or a runtime assertion rather than relying on method naming discipline ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/embedder_supervisor/mod.rs:157`) ⁽ᵐ⁾:
```
sidecar_batch_size: None,
            wedge_reset_secs: self.wedge_reset_secs,
```

95. **End-to-end proxy behaviour is explicitly untested in CI** — The proxy_handler's request-forwarding, SSRF guard ordering, retry, and body-limit logic are only unit-tested at the helper level, with no automated end-to-end coverage of the handler itself ⁽ⁱ⁾
- **Component:** crates/trusty-console/src/proxy/routes.rs:241
- **Business impact:** Regressions in the live proxy path (SSRF guard, retries, header filtering) could ship undetected since no CI test exercises the full handler ⁽ⁱ⁾
- **Remediation:** Add an in-process integration test that mocks an upstream daemon and drives proxy_handler through axum ⁽ⁱ⁾
- **Evidence** (`crates/trusty-console/src/proxy/routes.rs:241`) ⁽ᵐ⁾:
```
/// Test: URL construction is unit-tested in `tests` below.  End-to-end proxy
/// behaviour requires a live daemon and is not tested in CI.
```

96. **Linear API token forwarded as Bearer via unvalidated string interpolation into GraphQL** — Every backend method relies on `self.graphql` for auth and constructs GraphQL query strings with `format!` interpolation of `ISSUE_FIELDS`, without any visible credential validation in this file ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/linear/backend.rs:29
- **Business impact:** If the auth-carrying `graphql` helper mishandles a missing/invalid API key, all Linear operations fail opaquely and secrets handling is not verifiable from this layer ⁽ⁱ⁾
- **Remediation:** Verify the sibling `graphql`/`resolve_team_id` helpers reject missing API keys early with a clear error and never log the token ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/linear/backend.rs:29`) ⁽ᵐ⁾:
```
async fn create_issue(&self, p: CreateIssueParams) -> Result<Issue> {
        let team_id = self.resolve_team_id().await?;
```

97. **Linear GraphQL queries built with format!-interpolated field strings** — Response parsing across the whole backend silently coerces missing/malformed GraphQL fields to defaults (`unwrap_or_default`, `unwrap_or("")`) rather than surfacing partial-response or API errors ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/linear/backend.rs:82
- **Business impact:** A Linear API error or schema change yields empty/blank issues rather than a diagnosable failure, masking real integration breakage from operators ⁽ⁱ⁾
- **Remediation:** Inspect the GraphQL `errors` array on responses and return a hard error instead of defaulting silently on missing data fields ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/linear/backend.rs:82`) ⁽ᵐ⁾:
```
let nodes = v["data"]["team"]["states"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
```

98. **Worker loop holds both lane mutexes across await, serialising all workers** — Every worker must acquire the same two shared `Mutex`-wrapped receivers held across the `recv().await`, so with multiple workers only one can be waiting on the channels at a time ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/embed_pool.rs:397
- **Business impact:** The multi-worker autotune (2-4 workers on larger hosts) delivers less concurrency than advertised because receive is globally serialised on the shared receiver mutexes ⁽ⁱ⁾
- **Remediation:** Use a work-stealing/broadcast dispatch or per-worker channels so multiple workers can concurrently receive without contending on shared receiver locks ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/embed_pool.rs:397`) ⁽ᵐ⁾:
```
let mut interactive_guard = interactive_rx.lock().await;
            let mut background_guard = background_rx.lock().await;
            tokio::select! {
                biased;
```

99. **Pool reply timeout is a process-wide OnceLock cache, unadjustable at runtime** — The reply timeout is read once and cached in a process-global `OnceLock`, so the value read by the first caller is frozen for the entire process lifetime ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/embed_pool.rs:81
- **Business impact:** Operators cannot retune the embed reply timeout without restarting the daemon, and cross-test state leakage risk exists since the cache is process-global ⁽ⁱ⁾
- **Remediation:** Document the intentional immutability and ensure tests set the env var before any embed call, or make the value configurable via injected state ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/embed_pool.rs:81`) ⁽ᵐ⁾:
```
static CACHED: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let secs = std::env::var("TRUSTY_EMBED_POOL_REPLY_TIMEOUT_SECS")
```

100. **Worker count autotune silently defaults to 1 on RAM detection failure** — When RAM detection fails, the pool silently assumes 8 GB and drops to a single worker regardless of the host's true capacity ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/embed_pool.rs:458
- **Business impact:** A large-memory host with a RAM-detection glitch would run at minimum concurrency, degrading indexing/search throughput without any operator signal ⁽ⁱ⁾
- **Remediation:** Log a warning on RAM-detection failure so the conservative single-worker fallback is visible in operations ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/embed_pool.rs:458`) ⁽ᵐ⁾:
```
let ram_mb = crate::core::memory_policy::detect_total_ram_mb().unwrap_or(8 * 1024);
```

101. **Delegations can enter Running with no guaranteed terminal signal** — By design a delegation may become `Running` and never receive a terminal event, relying entirely on a 60 s sweep to eventually mark it `Stale` ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/services/delegation_tracker.rs:82
- **Business impact:** Stuck live delegations can suppress idle detection until the reap loop fires, and any bug in `sweep_delegations` (not in this batch) would leak in-flight state indefinitely ⁽ⁱ⁾
- **Remediation:** Confirm the `sweep_delegations` reaper is robust and covered by tests, and bound the maximum time a delegation may remain non-terminal ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/services/delegation_tracker.rs:82`) ⁽ᵐ⁾:
```
a delegation can enter `Running` with no
//! route out: `SubagentStop` is not guaranteed to arrive (`tm hook` POSTs fail
//! open on a 2 s budget, an interrupted subagent emits no stop, a dispatch that
```

102. **Sidebar API client swallows all fetch errors and returns empty arrays** — Both `fetchWorkstreams` and `fetchWorkstreamHistory` catch every error (network, auth 401, JSON parse) and return an empty array with no logging or user signal ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src/lib/workstreams.ts:45
- **Business impact:** An expired token or backend outage renders the workstream panel silently empty, indistinguishable from genuinely having no workstreams, hindering diagnosis ⁽ⁱ⁾
- **Remediation:** Distinguish auth/network failure from empty results and surface a subtle error indicator while still not crashing the panel ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src/lib/workstreams.ts:45`) ⁽ᵐ⁾:
```
if (!r.ok) return [];
    return (await r.json()) as WorkstreamSummary[];
  } catch {
    return [];
  }
```

103. **Admin stop endpoint trusts every caller with no authentication** — The graceful-shutdown admin endpoint accepts any request without authentication, relying solely on localhost binding for protection ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/admin.rs:74
- **Business impact:** Any local process (including malicious or compromised code) can shut down the search daemon, and if the bind assumption ever breaks the exposure widens ⁽ⁱ⁾
- **Remediation:** Require a bearer token or local socket authentication for admin/config mutation endpoints even on localhost ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/admin.rs:74`) ⁽ᵐ⁾:
```
The daemon is
/// localhost-only and trusts every caller, so no auth is required.
```

104. **Config mutation endpoints have no authentication** — The PATCH /config handler mutates runtime memory limits with no auth check on the caller ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/server/admin.rs:187
- **Business impact:** Any local caller can retune or disable daemon memory limits, potentially destabilizing the process ⁽ⁱ⁾
- **Remediation:** Gate config-mutation routes behind the same auth mechanism used elsewhere ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/server/admin.rs:187`) ⁽ᵐ⁾:
```
pub(super) async fn patch_config_handler(
    State(_state): State<Arc<SearchAppState>>,
    Json(req): Json<PatchConfigRequest>,
) -> Json<ConfigResponse> {
```

105. **Detached indexing threads are never joined and swallow all failures** — Background worktree indexing spawns detached OS threads whose outcome is only logged, never propagated or retried ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/core/worktree_index.rs:189
- **Business impact:** Silent indexing failures leave worktrees unsearchable, producing confusing 404 fallbacks with no operator signal beyond a warn log ⁽ⁱ⁾
- **Remediation:** Track outcomes via a metric or health surface so persistent registration failures are observable and retryable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/core/worktree_index.rs:189`) ⁽ᵐ⁾:
```
pub(super) fn spawn_detached<F: FnOnce() + Send + 'static>(work: F) {
    std::thread::spawn(work);
}
```

106. **Catch-up fails open on every source, potentially masking data loss** — The blocking catch-up wrapper returns an empty string on runtime build failure or thread panic, silently degrading operator context ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/catchup/mod.rs:328
- **Business impact:** Operators may resume sessions believing there is no new activity when catch-up simply failed, losing awareness of commits or sessions ⁽ⁱ⁾
- **Remediation:** Surface an explicit degraded-mode marker in the returned digest rather than an empty string ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/catchup/mod.rs:328`) ⁽ᵐ⁾:
```
runs [`run_catchup`] on it, and joins. On any error returns an empty string
/// (fail-open — catch-up must never abort session start).
```

107. **Global env-var mutation forces serialized tests and unsafe blocks** — Backend selection reads a process-global env var, requiring a test mutex and unsafe set_var/remove_var to avoid flaky cross-test races ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/memory/mod.rs:211
- **Business impact:** Global mutable env state couples unrelated tests and risks nondeterministic behavior if backend selection is ever changed at runtime ⁽ⁱ⁾
- **Remediation:** Resolve backend once into an immutable config value passed to constructors instead of reading process-global env repeatedly ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/memory/mod.rs:211`) ⁽ᵐ⁾:
```
static ENV_GUARD: Mutex<()> = Mutex::new(());
```

108. **Scaffolded memory module ships dead-code-allowed unused public surface** — The memory storage module is scaffolded ahead of consumers with dead_code suppressed, so much of its public surface has no live callers ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/memory/mod.rs:14
- **Business impact:** Untested-in-production code paths may harbor latent bugs that only surface when consumers are finally wired in ⁽ⁱ⁾
- **Remediation:** Track scaffolded modules and add integration coverage before enabling consumers, removing the blanket dead_code allow as callers land ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/memory/mod.rs:14`) ⁽ᵐ⁾:
```
#![allow(dead_code)]` below keeps the build clean until PM/agents start
//! calling it; unit tests in `redb_usearch.rs` exercise the public surface.
```

109. **SSE event serialization silently discards events on JSON failure** — If event serialization fails, `unwrap_or_default()` emits an empty string instead of surfacing or logging the error ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/api/control_routes.rs:323
- **Business impact:** Consumers may receive silent empty SSE frames and lose session state visibility without any diagnostic, complicating incident triage ⁽ⁱ⁾
- **Remediation:** Log the serialization failure and either skip or emit a structured error event rather than an empty default string ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/api/control_routes.rs:323`) ⁽ᵐ⁾:
```
let json = serde_json::to_string(&event).unwrap_or_default();
                Some(Ok(Event::default().data(json)))
```

110. **git worktree detection shells out to `git` on every binding resolve** — Every project binding resolution spawns a synchronous external `git` process, blocking the calling thread ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/binding/mod.rs:311
- **Business impact:** Under high session/task creation volume the process spawns add latency and load, and a slow/hung git invocation blocks the resolving thread ⁽ⁱ⁾
- **Remediation:** Cache worktree classification per canonical root or use a non-forking git library, and avoid running the blocking command on async worker threads ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/binding/mod.rs:311`) ⁽ᵐ⁾:
```
Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
```

111. **List handler acquires a metadata read lock per session in a sequential loop** — The list endpoint iterates all session IDs serially, doing a registry lookup and metadata read-lock per session ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/api/control_routes.rs:414
- **Business impact:** With many live sessions the list latency grows linearly and contends with concurrently updating sessions, degrading control-plane responsiveness ⁽ⁱ⁾
- **Remediation:** Snapshot metadata in a single registry pass or bound the number of sessions returned per request ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/api/control_routes.rs:414`) ⁽ᵐ⁾:
```
for id in ids {
        if let Some(handle) = state.session_registry.get(&id).await {
            let meta = handle.metadata.read().await;
```

112. **Reindex silently skips orphaned vector keys mid-search without diagnostics** — Search results silently drop HNSW keys that have no `key_to_id` mapping, with no metric or log recording how often this happens ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/store/usearch_impl.rs:99
- **Business impact:** Persistent id/key map divergence would silently shrink recall with no observable signal, making state-drift bugs hard to detect ⁽ⁱ⁾
- **Remediation:** Emit a debug/warn log or counter when orphaned keys are encountered so divergence between the graph and id maps is observable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/store/usearch_impl.rs:99`) ⁽ᵐ⁾:
```
// Silently skip orphaned keys (e.g. removed mid-search) — the alternative
            // of erroring would tear down a valid query for a benign race.
```

113. **Bearer token pulled from a global store into every cost request header** — The API token is read from a client-side store and sent as a Bearer header, and when absent the request proceeds unauthenticated rather than failing fast ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src/lib/costs.ts:63
- **Business impact:** A missing or leaked client-side token could permit unauthenticated cost queries or expose credentials in browser memory/logs ⁽ⁱ⁾
- **Remediation:** Refuse the request when no token is present and confirm the token store is not persisted in a way exposable to XSS ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src/lib/costs.ts:63`) ⁽ᵐ⁾:
```
function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}
```

114. **Cross-store consistency depends on documented crash-window residue, not true atomicity** — The redb metadata and usearch vector stores cannot share a transaction, so moves/inserts rely on ordering to leave only "inert" residue after a crash ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/memory/redb_usearch/store_impl.rs:176
- **Business impact:** Repeated crashes could accumulate orphaned vectors or leave half-visible memories, degrading search correctness over time ⁽ⁱ⁾
- **Remediation:** Add a periodic reconciliation/GC pass that reclaims orphan vectors and detect divergence, and cover the crash windows with fault-injection tests ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/memory/redb_usearch/store_impl.rs:176`) ⁽ᵐ⁾:
```
Not
        // achieved: full cross-store atomicity. A crash between the commit and
        // the source flush leaves a stale vector in the source index —
```

115. **Post-commit vector cleanup failures are only logged, silently orphaning data** — A failed source-vector removal after commit is downgraded to a warn log and the move reports success, so orphaned vectors accumulate without caller visibility ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/memory/redb_usearch/store_impl.rs:263
- **Business impact:** Silent orphan accumulation is invisible to callers and cannot trigger remediation, causing gradual index bloat and inconsistency ⁽ⁱ⁾
- **Remediation:** Track and surface cleanup-failure counts (metric or health signal) so orphans can be detected and reclaimed proactively ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/memory/redb_usearch/store_impl.rs:263`) ⁽ᵐ⁾:
```
tracing::warn!(
                id, label = src_label, error = %e,
                "move committed but the source vector could not be removed"
            );
```

116. **`list` degrades a reload failure to stale in-memory data without surfacing it to callers** — On a store reload I/O error the list endpoint returns the last-known in-memory records and only logs, so callers receive stale data with no error signal ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/session_manager/manager.rs:368
- **Business impact:** Operators or the supervisor may act on stale fleet state during persistent storage faults without any indication the data is unreliable ⁽ⁱ⁾
- **Remediation:** Expose reload-fallback state to callers (e.g. a staleness flag or health endpoint) rather than only logging it ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/session_manager/manager.rs:368`) ⁽ᵐ⁾:
```
let last_known = guard.cached_all();
                super::store_health::log_reload_fallback(&e, last_known.len());
                last_known
            }
```

117. **High-cyclomatic session manager hotspot concentrates lifecycle logic** — The session manager module is flagged as a cyclomatic-84 hotspot and repeatedly splits code into sibling files to stay under the 500-SLOC cap, indicating heavy branching concentrated in one component ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/session_manager/manager.rs:11
- **Business impact:** High complexity in the daemon's authoritative lifecycle component raises maintenance risk and the chance of state-transition bugs as the fleet scales ⁽ⁱ⁾
- **Remediation:** Decompose the create/resume/reconcile state machine into smaller testable units and reduce branching in the hotspot ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/session_manager/manager.rs:11`) ⁽ᵐ⁾:
```
//! `mark_reactivated` (the in-place counterpart to `resume`, #2023 C) lives in
//! the sibling `reactivate.rs`, and `reconcile_on_boot` lives in the sibling
//! `reconcile.rs` (#2379) — both extracted to keep this file under the
```

118. **Session store guarded by a single async RwLock with reload-on-read using write locks** — Read paths like `get` and `list` acquire a write lock to perform reload-if-changed, serializing all reads through an exclusive lock on the shared store ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/session_manager/manager.rs:360
- **Business impact:** Under many concurrent sessions or frequent polling, forcing reads through a write lock can create contention and throughput bottlenecks in the daemon ⁽ⁱ⁾
- **Remediation:** Separate the change-detection/reload path from read serving, or use a lock-free/snapshot read path so reads do not require exclusive locks ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/session_manager/manager.rs:360`) ⁽ᵐ⁾:
```
pub async fn list(&self) -> Vec<SessionRecord> {
        let mut guard = self.store.write().await;
```

119. **Live OAuth browser round-trip is explicitly untested** — The end-to-end interactive OAuth consent and token-exchange flow that mints on-disk tokens is deferred from automated testing, leaving only pure helpers unit-tested ⁽ⁱ⁾
- **Component:** crates/trusty-gworkspace/src/api/auth/oauth/mod.rs:15
- **Business impact:** Untested minting of credential tokens risks silent regressions in the auth path that would only surface in production onboarding ⁽ⁱ⁾
- **Remediation:** Add wiremock/stubbed integration coverage for the full consent-to-persist sequence to exercise token minting without real Google credentials ⁽ⁱ⁾
- **Evidence** (`crates/trusty-gworkspace/src/api/auth/oauth/mod.rs:15`) ⁽ᵐ⁾:
```
the live browser round-trip is deferred (needs real Google creds).
```

120. **OAuth token exchange sends client_secret in POST body over the reqwest default client** — The client secret and authorization code are transmitted to the token endpoint using a plain reqwest client with no explicit TLS or timeout enforcement in this call site ⁽ⁱ⁾
- **Component:** crates/trusty-gworkspace/src/api/auth/oauth/flow/mod.rs:434
- **Business impact:** A secret leak or hung request could expose long-lived Google credentials or stall the setup flow indefinitely ⁽ⁱ⁾
- **Remediation:** Use a shared client with an explicit timeout and confirm TLS enforcement for the token exchange path ⁽ⁱ⁾
- **Evidence** (`crates/trusty-gworkspace/src/api/auth/oauth/flow/mod.rs:434`) ⁽ᵐ⁾:
```
let params = [
        ("client_id", creds.client_id.as_str()),
        ("client_secret", creds.client_secret.as_str()),
        ("code", code),
```

121. **Bearer tokens read from a client-side store and injected into fetch headers** — The API token is stored in a browser-accessible store and attached to every request, exposing it to XSS-based exfiltration if tokens are held in JS-reachable state ⁽ⁱ⁾
- **Component:** crates/trusty-agents/ui/src/lib/agentConfig.ts:26
- **Business impact:** A compromised UI dependency or XSS could steal the agent-config API token and drive config-mutating PATCH calls ⁽ⁱ⁾
- **Remediation:** Confirm the token store is not persisted in localStorage and consider httpOnly cookie-based auth for the sidecar ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/ui/src/lib/agentConfig.ts:26`) ⁽ᵐ⁾:
```
function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}
```

122. **PR history source issues up to ~50 sequential HTTP requests per review with no rate-limit handling** — The gather loop fetches commits per file then serially fetches PRs per commit SHA with no concurrency, backoff, or GitHub rate-limit awareness beyond a fixed 15s timeout ⁽ⁱ⁾
- **Component:** crates/trusty-review/src/integrations/context/pr_history.rs:600
- **Business impact:** Enabling this source at scale could exhaust GitHub API quotas or add serial latency to every review ⁽ⁱ⁾
- **Remediation:** Add rate-limit detection/backoff and parallelize per-SHA lookups with a bounded concurrency limit ⁽ⁱ⁾
- **Evidence** (`crates/trusty-review/src/integrations/context/pr_history.rs:600`) ⁽ᵐ⁾:
```
let prs_body = match self
                    .transport
                    .fetch_prs_for_commit(&token, &subject.owner, &subject.repo, sha)
                    .await
                {
```

123. **In-conversation action execution depends on best-effort palace dual-write that silently no-ops** — Conversation turns and executed actions are persisted only via an in-memory window plus a best-effort palace write that silently drops on failure, risking lost audit history of mutating confirm turns ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/manager/chat.rs:270
- **Business impact:** A restart or palace outage loses the record of who confirmed a session launch/inject, weakening state durability and auditability ⁽ⁱ⁾
- **Remediation:** Surface or retry palace write failures for confirm turns rather than silently ignoring them ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/manager/chat.rs:270`) ⁽ᵐ⁾:
```
manager
            .palace()
            .record_chat_turn(&conversation_key, &body.message, &reply)
            .await;
```

124. **Pending proposal store is unconditionally consumed, dropping a valid proposal on any interleaved turn** — The `take` consumes the pending proposal even when the message is not a confirmation, so any non-confirming interleaved message permanently discards the proposal (next-turn-only TTL by design but brittle for concurrent clients) ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/manager/chat.rs:250
- **Business impact:** Concurrent messages on the same conversation key can silently lose a proposal, forcing users to re-request actions ⁽ⁱ⁾
- **Remediation:** Only consume the proposal when the message is a confirmation, or document/lock concurrent access on the key ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/manager/chat.rs:250`) ⁽ᵐ⁾:
```
if let Some(pending) = manager.proposals().take(&conversation_key)
        && is_confirmation(&body.message)
```

125. **Process-global mutable state via OnceLock for canonicalize failure tracking** — Failure-streak state is held in a process-global mutex-guarded map rather than on the SessionManager struct, coupling multiple call sites to hidden shared state ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/session_manager/prune.rs:152
- **Business impact:** Global mutable state complicates reasoning about test isolation and multi-instance/concurrent daemon deployments where per-instance scoping would be expected ⁽ⁱ⁾
- **Remediation:** Move the streak counter onto the SessionManager instance so its lifetime and scope match the daemon it belongs to ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/session_manager/prune.rs:152`) ⁽ᵐ⁾:
```
static CANONICALIZE_FAILURE_STREAKS: std::sync::OnceLock<
    std::sync::Mutex<CanonicalizeFailureStreaks>,
> = std::sync::OnceLock::new();
```

126. **Bulk prune of managed sessions is unbounded in-memory snapshot** — prune_managed snapshots the entire session store into memory and iterates per-record, which scales linearly with the accumulated record count that motivated this feature (239+ stale records) ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/session_manager/prune.rs:426
- **Business impact:** As the store grows a full-snapshot-then-per-record-resave pass becomes slow and memory-heavy, degrading the very cleanup path meant to fix unbounded growth ⁽ⁱ⁾
- **Remediation:** Consider batching or streaming records rather than loading and rewriting the full set in one pass ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/session_manager/prune.rs:426`) ⁽ᵐ⁾:
```
// Snapshot the full set ONCE (reloads-on-read so out-of-process writes are
        // seen). We then mutate per record below, each of which re-reads/saves.
```

127. **Cross-palace prompt-fact retraction swallows per-palace errors** — handle_remove_prompt_fact logs and continues on any per-palace retract failure, so the returned success/removed count can mask palaces where retraction actually failed ⁽ⁱ⁾
- **Component:** crates/trusty-memory/src/tools/kg_ops.rs:219
- **Business impact:** A caller may believe a prompt fact was fully removed when it silently persists in a palace that errored, leaving stale facts injected into prompts ⁽ⁱ⁾
- **Remediation:** Surface a partial-failure indicator in the response so callers can distinguish full removal from best-effort with errors ⁽ⁱ⁾
- **Evidence** (`crates/trusty-memory/src/tools/kg_ops.rs:219`) ⁽ᵐ⁾:
```
Err(e) => tracing::warn!(
                    palace = %palace_id.as_str(),
                    "retract failed: {e:#}",
                ),
```

128. **Permanent-error classification relies on brittle string matching** — Bot-exit classification matches lowercased error-message substrings, so any upstream wording change silently reclassifies a permanent misconfiguration as transient and tight-loops around it ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/telegram/supervisor.rs:58
- **Business impact:** A dependency or Telegram API message change could make a doomed token error be retried forever, spamming logs and never giving up cleanly ⁽ⁱ⁾
- **Remediation:** Classify on typed error variants or error codes rather than free-form message substrings ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/telegram/supervisor.rs:58`) ⁽ᵐ⁾:
```
let permanent = msg.contains("token is required")
        || msg.contains("invalid token")
        || msg.contains("not a valid bot token")
        || msg.contains("unauthorized");
```

129. **Corrupt drawer table silently falls back to empty at palace open** — A failed load of the authoritative redb drawer table degrades silently to L1-only, potentially discarding most of a palace's memory state without failing the open ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/memory_core/retrieval/handle.rs:321
- **Business impact:** Data-integrity errors are masked as warnings, so a corrupted store can lose the majority of stored memories while the palace still appears operational ⁽ⁱ⁾
- **Remediation:** Distinguish transient from corruption errors and fail loudly (or quarantine) rather than always falling back to empty ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/memory_core/retrieval/handle.rs:321`) ⁽ᵐ⁾:
```
Err(e) => {
                tracing::warn!(palace = %palace.id, "load_drawers failed, falling back to L1 only: {e:#}");
                Vec::new()
            }
```

130. **Dotenv token resolution can log token-bearing errors and reads plaintext secrets** — Bot tokens and API keys are read from plaintext dotenv files in the working directory rather than a secret manager ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/telegram/mod.rs:146
- **Business impact:** Plaintext credential files are easily leaked via backups, container images, or accidental commits, exposing operator control of the bot ⁽ⁱ⁾
- **Remediation:** Support a secrets backend or OS keychain and document rotation for the dotenv fallback ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/telegram/mod.rs:146`) ⁽ᵐ⁾:
```
pub fn resolve_token(var_name: &str) -> Option<String> {
    for file in [".env.local", ".env"] {
        if let Some(value) = read_dotenv_key(Path::new(file), var_name) {
```

131. **High-complexity keyword extractor with brittle substring routing (cyclomatic 79)** — A single 79-cyclomatic function chains dozens of fragile substring heuristics for role/language routing that are easy to break with edits ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/inspection/task_signals.rs:72
- **Business impact:** High-complexity heuristic routing is hard to maintain and silently misroutes tasks as keyword phrasing evolves ⁽ⁱ⁾
- **Remediation:** Refactor into a data-driven keyword table with per-rule tests and reduce branching ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/inspection/task_signals.rs:72`) ⁽ᵐ⁾:
```
if lower.contains("golang")
            || lower.contains(" go ")
            || lower.starts_with("go ")
```

132. **Chat history mutex uses expect() that panics on poison in handler path** — The shared per-chat history mutex is unwrapped with expect, so any prior panic while holding the lock cascades into every subsequent handler ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/telegram/mod.rs:483
- **Business impact:** A single panicking handler can wedge the Telegram bot's shared state and abort further message processing ⁽ⁱ⁾
- **Remediation:** Recover the poisoned guard as done in corpus_fault's lock() rather than panicking ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/telegram/mod.rs:483`) ⁽ᵐ⁾:
```
let guard = histories.lock().expect("chat history mutex poisoned");
```

133. **Secrets synced via untracked-file patterns default to ENABLED** — The untracked-file sync feature defaults to copying `.env*` files (which routinely hold plaintext secrets) into managed session workspaces with no explicit operator opt-in ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/core/trusty_tools_config.rs:161
- **Business impact:** Secrets in `.env` files may be propagated into worktrees or agent-visible locations by default, widening the exposure surface for leaked credentials ⁽ⁱ⁾
- **Remediation:** Default untracked/secret sync to disabled and require explicit operator opt-in for any pattern that can match secret files ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/core/trusty_tools_config.rs:161`) ⁽ᵐ⁾:
```
/// `None` → the built-in default applies: sync is ENABLED with the
    /// built-in `.env*` pattern set (see [`resolve_untracked_sync`]). A
    /// per-project `untracked_sync` override (on [`ProjectConfig`]) takes
```

134. **Positional catch-up offset can silently repeat records under concurrency** — Paging over catch-up records uses a positional offset into a filesystem-rebuilt list, so concurrent snapshot writes shift indexes and cause a later page to repeat a record ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/daemon/catchup_bounds.rs:28
- **Business impact:** A resuming session may receive duplicated or misaligned state, degrading correctness of the resume feature under concurrent activity ⁽ⁱ⁾
- **Remediation:** Adopt a stable cursor (e.g. a monotonic id or content hash) that survives mid-walk mutation rather than a positional index ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/daemon/catchup_bounds.rs:28`) ⁽ᵐ⁾:
```
A snapshot written mid-walk sorts to the front
//! and shifts every index, so a later page can REPEAT a record. That is
//! disclosed on the response rather than engineered away, and the alternatives
```

135. **Alternate-screen default flip has server-wide side effects on all panes** — The `alternate_screen: false` default is applied to a single shared tmux server, so it alters every pane's screen-restore behaviour across all managed sessions rather than scoping per-session ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/core/trusty_tools_config.rs:293
- **Business impact:** Unrelated user tools (vim, less, htop) in any managed session lose screen restoration and leave redraw garbage, causing UX regressions the operator did not opt into ⁽ⁱ⁾
- **Remediation:** Scope tmux screen options per-session or per-window rather than server-wide, or make the default opt-in ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/core/trusty_tools_config.rs:293`) ⁽ᵐ⁾:
```
The effect is
    /// server-wide, and every trusty-* managed session shares ONE tmux server,
    /// so this changes every pane on it, not just the ones running an agent.
```

136. **Env-driven embedder tuning cached in a process-global OnceLock** — In-flight concurrency and call timeout are read from env once and cached in a process-global `OnceLock`, so the value is fixed for the process lifetime and cannot be reconfigured without restart ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/embedder_client/stdio.rs:141
- **Business impact:** Operators cannot tune embedder concurrency at runtime, limiting scaling responsiveness and complicating test isolation across the shared cache ⁽ⁱ⁾
- **Remediation:** Pass tuning parameters through explicit configuration/constructor arguments instead of a process-wide cached env read ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/embedder_client/stdio.rs:141`) ⁽ᵐ⁾:
```
static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("TRUSTY_EMBED_INFLIGHT")
```

137. **Verify-tail polling is a fixed ~120s aggregate cap with sequential per-member folds** — The verify tail folds sequentially over every daemon member and blocks the installer up to a hardcoded ~120s aggregate budget rather than probing concurrently ⁽ⁱ⁾
- **Component:** crates/trusty-installer/src/commands/verify_tail.rs:116
- **Business impact:** Installs with many stuck daemons stall CI/automation for minutes, degrading the operator experience the module was built to improve ⁽ⁱ⁾
- **Remediation:** Probe members concurrently or make the budget configurable to bound wall-clock time as the number of daemons grows ⁽ⁱ⁾
- **Evidence** (`crates/trusty-installer/src/commands/verify_tail.rs:116`) ⁽ᵐ⁾:
```
const AGGREGATE_POLL_BUDGET: Duration = Duration::from_secs(120);
```

138. **MCP client plugin log path silently swallows I/O errors and falls back to null** — Directory creation failures for the plugin stderr log are discarded and log output is silently dropped to null on open failure ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/stdio_mcp_client/mod.rs:85
- **Business impact:** Operators debugging a misbehaving MCP plugin may find no logs at all with no indication the log setup itself failed ⁽ⁱ⁾
- **Remediation:** Surface directory-creation failures at warn level rather than ignoring the Result ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/stdio_mcp_client/mod.rs:85`) ⁽ᵐ⁾:
```
let _ = std::fs::create_dir_all(&base);
```

139. **Plugin log path uses process-global env HOME with no isolation across concurrent spawns** — A process-global mutex-guarded set tracks announced plugin paths, meaning dedup state is shared across all clients in the process ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/stdio_mcp_client/mod.rs:49
- **Business impact:** In a long-lived daemon serving many contexts, the global set grows unbounded and cannot be reset per-session ⁽ⁱ⁾
- **Remediation:** Scope the announced-plugins state per client or add eviction to bound growth ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/stdio_mcp_client/mod.rs:49`) ⁽ᵐ⁾:
```
fn announced_plugins() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
```

140. **Kickstart failure return value is ambiguous and cannot distinguish repair from failure** — The launchctl kickstart helper collapses spawn errors and non-zero exits into the same `false`, discarding the distinction between a missing binary and a genuinely failed force-start ⁽ⁱ⁾
- **Component:** crates/trusty-installer/src/commands/verify_tail.rs:338
- **Business impact:** Operators lose diagnostic detail on why a repair attempt failed, complicating triage of stuck daemons ⁽ⁱ⁾
- **Remediation:** Propagate the underlying command error or capture stderr rather than mapping every failure mode to a bare bool ⁽ⁱ⁾
- **Evidence** (`crates/trusty-installer/src/commands/verify_tail.rs:338`) ⁽ᵐ⁾:
```
.status()
        .map(|s| s.success())
        .unwrap_or(false)
```

141. **gh token stripping relies on hardcoded two-variable list** — Only two token env vars are neutralised before enforcing the active gh account, so any other credential-carrying variable gh honours would bypass the config-dir isolation ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/core/gh_account_enforce.rs:73
- **Business impact:** A future or alternate gh auth env variable could silently outrank the isolated config dir and redirect operations to the wrong identity ⁽ⁱ⁾
- **Remediation:** Cross-check against gh's documented auth env precedence and add a defensive check plus a verify-time refusal for any recognised override ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/core/gh_account_enforce.rs:73`) ⁽ᵐ⁾:
```
const GH_TOKEN_ENV_VARS: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];
```

142. **SurfaceWalker is a single ~220-line function with cyclomatic complexity 79** — The public-surface walker concentrates high branching complexity in one class that two gates depend on, with test coverage delegated entirely to shell selftests ⁽ⁱ⁾
- **Component:** scripts/lib/rustdoc_walk.py:193
- **Business impact:** A high-complexity shared traversal is hard to change safely and a subtle regression could silently break both semver and contract gates ⁽ⁱ⁾
- **Remediation:** Add unit-level Python tests for the walker's branches and consider decomposing the largest methods ⁽ⁱ⁾
- **Evidence** (`scripts/lib/rustdoc_walk.py:193`) ⁽ᵐ⁾:
```
class SurfaceWalker:
    """Walks the public surface from the crate root, yielding item positions.
```

143. **Process-global mutable state in deferred-embed queue limits multi-tenant isolation** — The catch-up queue and depth counters are process-global singletons, so all indexes across the process share one heap and cannot be isolated per-tenant or per-runtime ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/reindex/defer_embed_queue.rs:143
- **Business impact:** A single global queue couples every index's catch-up scheduling and complicates scaling to isolated or sharded worker processes ⁽ⁱ⁾
- **Remediation:** Consider parameterizing the queue by a scheduler instance passed through call sites rather than a hidden static ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/reindex/defer_embed_queue.rs:143`) ⁽ᵐ⁾:
```
fn queue_heap() -> &'static Mutex<BinaryHeap<QueuedEmbedJob>> {
    static HEAP: OnceLock<Mutex<BinaryHeap<QueuedEmbedJob>>> = OnceLock::new();
```

144. **O(n) heap rebuild on every winning poll tick scales poorly with many pending indexes** — Each waiting task polls the shared heap every 20ms and, on winning, tears the whole heap into a vec and rebuilds it, an O(n) operation whose cost grows with pending indexes (documented as hundreds) ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/reindex/defer_embed_queue.rs:322
- **Business impact:** With large fleets of colocated indexes the polling and rebuild cost, plus per-job spawned tasks, adds latency and CPU churn during warm-boot bursts ⁽ⁱ⁾
- **Remediation:** Replace polling with a notify/wake mechanism and an O(log n) targeted removal keyed by seq ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/reindex/defer_embed_queue.rs:322`) ⁽ᵐ⁾:
```
let items = std::mem::take(&mut *heap).into_vec();
                let mut items = items;
                let pos = items
                    .iter()
                    .position(|j| j.seq == my_seq)
```

145. **gh CLI ticketing backend depends on external credential state it does not control** — Authentication is delegated entirely to an external `gh` CLI process whose auth state (and any tokens it holds) is opaque to and unmanaged by this codebase ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/gh_cli/mod.rs:39
- **Business impact:** Reliance on an out-of-band CLI credential means auth failures, token scope, and revocation are invisible to the app, complicating operational reliability and audit ⁽ⁱ⁾
- **Remediation:** Surface gh auth scope/expiry state where possible and document the credential trust boundary for operators ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/gh_cli/mod.rs:39`) ⁽ᵐ⁾:
```
pub async fn gh_available() -> bool {
    // #5475: the probe now lives in trusty-common's `gh` entry point.
    trusty_common::gh::gh_available().await
}
```

146. **Network/CLI paths in gh ticketing backend are untested** — The actual subprocess invocation, argv assembly execution, and error mapping against a real or mock `gh` are explicitly not covered by tests ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/ticketing/gh_cli/mod.rs:14
- **Business impact:** Regressions in the spawn/parse path for real `gh` output could ship undetected, breaking the ticketing integration in production ⁽ⁱ⁾
- **Remediation:** Add a fakeable command runner seam so `run`/`run_with_repo` and JSON parsing can be exercised with recorded fixtures ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/ticketing/gh_cli/mod.rs:14`) ⁽ᵐ⁾:
```
//! Network/CLI calls are not exercised in unit tests (env-dependent).
```

147. **Best-effort refresh silently swallows RPC errors leaving stale caches** — An RPC failure during workstream refresh is discarded via `.ok()?`, returning None and leaving stale cache contents with no surfaced error to the user ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/tui_client/engine_state.rs:151
- **Business impact:** Users may see silently outdated workstream lists after a transient daemon failure without any indication a refresh failed ⁽ⁱ⁾
- **Remediation:** Emit a status/connection event to the TUI on refresh failure while still preserving the stale cache ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/tui_client/engine_state.rs:151`) ⁽ᵐ⁾:
```
let result = self.rpc.call("workstream.list", json!({})).await.ok()?;
```

148. **CoreML embedding backend intermittently emits NaN/zero vectors that poison the index** — A dependency (CoreML EP) intermittently produces degenerate embeddings that would poison every subsequent nearest-neighbour query if not screened out ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/store/usearch_store.rs:240
- **Business impact:** Reliance on an unstable third-party inference provider risks silent search-quality degradation if the validation guard is ever bypassed or a new code path skips it ⁽ⁱ⁾
- **Remediation:** Ensure validate_embedding is invoked on every insert path and add regression tests for any new upsert entry points ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/store/usearch_store.rs:240`) ⁽ᵐ⁾:
```
the CoreML execution provider intermittently emits NaN
/// or all-zero embedding vectors for a small fraction of chunks. usearch's
/// cosine metric divides by the vector norm, so an all-zero vector yields
```

149. **HNSW max_elements cap enforced in code because usearch API lacks the field** — The 1M-vector cap is a hard ceiling enforced manually and, combined with ~243 resident indexes per host, memory scaling is bounded by manual heuristics rather than the backend ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/core/store/usearch_store.rs:69
- **Business impact:** Large customer repositories exceeding the cap will hit insert errors, and the multi-index memory model may not scale to larger deployments without re-tuning ⁽ⁱ⁾
- **Remediation:** Make the cap configurable per index and document scaling limits for large-monorepo deployments ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/core/store/usearch_store.rs:69`) ⁽ᵐ⁾:
```
once the index would grow past
/// this many vectors, subsequent inserts return an error so the daemon can
/// bound RAM (~6 GB at 1M × 384-dim × 4 bytes plus graph overhead).
```

150. **Focus/proxy state is entirely in-memory and volatile across daemon restart** — Per-conversation focus mapping is stored only in a Mutex-guarded HashMap with no persistence, so any daemon restart silently drops all conversation focus state ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/client/proxy.rs:26
- **Business impact:** A daemon restart or crash silently resets every operator's focused session context, degrading UX for external channel users mid-conversation ⁽ⁱ⁾
- **Remediation:** Consider persisting focus state or at least surfacing a notification to channels on reset if this becomes user-visible friction ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/client/proxy.rs:26`) ⁽ᵐ⁾:
```
Focus state is in-memory and VOLATILE across a
//! daemon restart (no channel persists per-conversation state); a restart simply
//! drops every conversation back to fleet-wide chat.
```

151. **Session-manager auto-unfocus relies on fragile substring matching of error text** — The distinction between a vanished session and a transient transport failure hinges on matching the literal substring "not found" in an untyped error string ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/client/proxy.rs:505
- **Business impact:** An upstream wording change to error messages could silently disable the auto-unfocus safety net or misclassify transient failures, causing wrong routing behavior ⁽ⁱ⁾
- **Remediation:** Replace substring matching with a typed error variant signalling a missing session ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/client/proxy.rs:505`) ⁽ᵐ⁾:
```
Every [`ManagedBackend`] impl is contractually
/// required to phrase a resolution failure with the substring `"not found"` (the
/// [`CommandExecutor`] impl inherits this wording verbatim from the executor's
```

152. **AWS credential detection uses conservative env heuristic that misses native sources** — The auto-precedence chain probes only three env vars for AWS credentials and cannot see file/role/IMDS sources, so it may skip a reachable Bedrock provider ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/core/sm/providers/resolve.rs:372
- **Business impact:** Deployments relying on IAM roles or shared-credentials files silently fall through to a less-preferred provider or degraded mode unless operators manually pin Bedrock ⁽ⁱ⁾
- **Remediation:** Perform the async SDK credential probe (or document the pin requirement prominently) so auto-selection reflects actually-reachable providers ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/core/sm/providers/resolve.rs:372`) ⁽ᵐ⁾:
```
It deliberately
    /// does NOT detect AWS-native credential sources that the SDK chain resolves
    /// at call time — the shared-credentials file (`~/.aws/credentials`), an ECS
    /// task role, EKS IRSA, or EC2 IMDS. In those AWS-native deployments the
```

153. **Token estimation uses a crude 4-chars-per-token heuristic rather than a real tokenizer** — Context-window trimming budgets are computed from an approximate byte-based estimate, not the model's actual tokenizer ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/context/trim.rs:349
- **Business impact:** Under-estimation can push a trimmed conversation over the hard model context window and cause provider rejection or truncation failures at scale ⁽ⁱ⁾
- **Remediation:** Optionally use a per-model tokenizer for critical budget decisions or add a safety margin to the estimate ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/context/trim.rs:349`) ⁽ᵐ⁾:
```
((content_str.len() as u32) / 4).max(1)
```

154. **Index byte budget performs sequential fs::metadata stat over every file** — The byte-budget check synchronously stats each post-filter file one at a time on the calling thread ⁽ⁱ⁾
- **Component:** crates/trusty-search/src/service/index_budget.rs:164
- **Business impact:** For very large post-filter file lists the serial synchronous stat loop adds latency to every reindex request and can block the daemon worker ⁽ⁱ⁾
- **Remediation:** Reuse metadata already gathered during the walk or parallelize/short-circuit the stat pass ⁽ⁱ⁾
- **Evidence** (`crates/trusty-search/src/service/index_budget.rs:164`) ⁽ᵐ⁾:
```
let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            total = total.saturating_add(len);
```

155. **GitHub backend authenticated only by long-lived PAT** — The GitHub tickets backend relies on a personal access token model, which lacks the scoping and rotation guarantees of GitHub App installation tokens ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/tickets/api/backends/github/mod.rs:5
- **Business impact:** A leaked or over-scoped PAT grants broad, long-lived access to the customer's GitHub org until manually revoked ⁽ⁱ⁾
- **Remediation:** Support GitHub App installation tokens with narrow scopes and automatic rotation as an alternative to PATs ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/tickets/api/backends/github/mod.rs:5`) ⁽ᵐ⁾:
```
What: PAT-authenticated `reqwest` client. REST for issues/comments/
//! labels/milestones; GraphQL for Projects V2.
```

156. **Palace auto-create uses force:true bypassing metadata protection** — The turn recorder issues palace_create with force:true which the docs note is an authz-gate bypass for app-managed palaces, weakening the tenancy guard ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/session/memory_sink.rs:502
- **Business impact:** In a multi-tenant deployment this force flag could let the sink create/overwrite palaces it should not be authorized to touch ⁽ⁱ⁾
- **Remediation:** Gate force:true behind an explicit single-tenant assertion and validate the palace slug against the caller's project scope ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/session/memory_sink.rs:502`) ⁽ᵐ⁾:
```
let create_params = json!({
        "name": palace,
        "force": true,
        "description": "trusty-code session turn history (auto-created by the turn recorder)",
    });
```

157. **Reconcile silently swallows tmux listing failures** — A failure to list live tmux sessions is coerced to an empty list, so a transient tmux error would make reconcile mark every registry session orphaned and prune them ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/tm/manager.rs:374
- **Business impact:** A transient tmux hiccup could cause mass loss of durable session state via prune_orphaned, silently destroying tracked work ⁽ⁱ⁾
- **Remediation:** Distinguish 'no sessions' from 'tmux query failed' and skip the orphan/prune phase when the listing errors ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/tm/manager.rs:374`) ⁽ᵐ⁾:
```
let live = self.tmux.list_sessions().unwrap_or_default();
```

158. **GitHub client depends on env-supplied token with no rotation surface shown** — Retry treats 401/403 (auth/rate-limit exhaustion) as non-transient and returns them to the caller without a dedicated auth-failure path, delegating status handling to callers ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/collect/github/retry.rs:42
- **Business impact:** Expired or revoked tokens surface as generic non-transient responses, making auth failures hard to distinguish from other 4xx during operations ⁽ⁱ⁾
- **Remediation:** Classify 401/403 explicitly and emit a clear auth-failure error so operators can act on token issues ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/collect/github/retry.rs:42`) ⁽ᵐ⁾:
```
let transient = status.as_u16() == 429 || (500..=599).contains(&status.as_u16());
                if !transient || attempt == MAX_RETRIES {
                    return Ok(resp);
```

159. **Unbounded directory walk over Datadog JSON files** — The Datadog ingest reads every file in the configured directory into memory one-by-one inside a single transaction with no batching or size limits ⁽ⁱ⁾
- **Component:** crates/trusty-git-analytics/src/commands/incidents/mod.rs:244
- **Business impact:** A large export directory or oversized JSON files could hold the transaction open and consume excessive memory during ingest ⁽ⁱ⁾
- **Remediation:** Stream/parse with size caps and commit in batches rather than one all-encompassing transaction ⁽ⁱ⁾
- **Evidence** (`crates/trusty-git-analytics/src/commands/incidents/mod.rs:244`) ⁽ᵐ⁾:
```
for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
```

160. **Fire-and-forget memory sink silently drops turns on overflow** — When the bounded queue is full the newest turn is dropped with only a warn log, so durable conversation history can be lost under load ⁽ⁱ⁾
- **Component:** crates/trusty-code/src/session/memory_sink.rs:374
- **Business impact:** During bursts or a slow/unreachable trusty-memory daemon, session turns are permanently lost, undermining the 'durable session' guarantee ⁽ⁱ⁾
- **Remediation:** Add persistent spillover or backpressure signalling so dropped turns are recoverable or visible beyond a log line ⁽ⁱ⁾
- **Evidence** (`crates/trusty-code/src/session/memory_sink.rs:374`) ⁽ᵐ⁾:
```
Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    failure_category = "queue_full",
```

161. **Cross-process concurrency not guaranteed for workstream ledger writes** — The ledger only serializes writes within a single process via an in-memory Mutex, so two processes racing a read-modify-write can silently lose an update ⁽ⁱ⁾
- **Component:** crates/trusty-agents-common/src/workstreams/ledger.rs:33
- **Business impact:** Concurrent access from multiple processes can drop workstream records with no error, corrupting the registry the lead agent relies on ⁽ⁱ⁾
- **Remediation:** Add an fs4 advisory file lock (as trusty-agents::state_writer already does) or enforce the documented single-writer-per-process routing mechanically ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents-common/src/workstreams/ledger.rs:33`) ⁽ᵐ⁾:
```
does **not** guarantee safety across *multiple
//! processes* writing the same ledger file concurrently — the last writer's
//! `rename` wins and an interleaved reader sees either the old or new file,
```

162. **Publisher identity check depends on parsing gh CLI text output** — The publish identity gate relies on string-parsing `gh auth status` output, which is a brittle human-readable surface that can change between gh CLI versions ⁽ⁱ⁾
- **Component:** scripts/preflight-publish.sh:448
- **Business impact:** A change in gh CLI output format could silently break the identity check that exists specifically to prevent wrong-account publishes ⁽ⁱ⁾
- **Remediation:** Query gh's machine-readable API (gh api user) or a stable JSON output instead of parsing status text ⁽ⁱ⁾
- **Evidence** (`scripts/preflight-publish.sh:448`) ⁽ᵐ⁾:
```
identity_active_account_from_status "$gh_output"
  if [ "$IDENTITY_ACTIVE_ACCOUNT" = "bobmatnyc" ]; then
    echo "[PASS] identity: active gh account is bobmatnyc"
```

163. **Malformed prior tool-call arguments silently swallowed during conversion** — A tool call with malformed JSON arguments is silently replaced with an empty object rather than surfaced, so a corrupt prior call is sent to Bedrock as an empty invocation ⁽ⁱ⁾
- **Component:** crates/trusty-common/src/inference/bedrock/convert.rs:333
- **Business impact:** Silent argument loss can produce incorrect model behavior with no diagnostic, making conversion bugs hard to trace ⁽ⁱ⁾
- **Remediation:** Log or surface the parse failure so silent data loss is at least observable ⁽ⁱ⁾
- **Evidence** (`crates/trusty-common/src/inference/bedrock/convert.rs:333`) ⁽ᵐ⁾:
```
let parsed: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::Object(Default::default()));
```

164. **crates.io version-live check requires manual, unfixtured testing** — Half the publish-safety guardrail's checks are only exercised manually because they depend on live external services, leaving them without automated regression coverage ⁽ⁱ⁾
- **Component:** scripts/preflight-publish.sh:242
- **Business impact:** Undetected regressions in the network/identity checks could silently permit the exact wrong-account or burned-version publishes this script was built to stop ⁽ⁱ⁾
- **Remediation:** Introduce injectable transport/gh shims so checks 1-4 can be driven against recorded fixtures in CI ⁽ⁱ⁾
- **Evidence** (`scripts/preflight-publish.sh:242`) ⁽ᵐ⁾:
```
checks 1-4 are exercised manually — they are bound to the network, the
#   real crates.io registry, and the logged-in gh account, none of which a
#   fixture can stand in for. Check 5 has TWO: check_semver_selftest.sh drives
#   the delegated gate, and preflight-check5-selftest.sh drives THIS script's
```

165. **Best-effort fsync means latest ledger write can be lost on crash** — The parent-directory fsync completing the durable-rename pattern is best-effort (`let _ =`), so on platforms that ignore or reject directory fsync the most recent mutation can be lost on unclean shutdown ⁽ⁱ⁾
- **Component:** crates/trusty-agents-common/src/workstreams/ledger.rs:316
- **Business impact:** An acknowledged workstream mutation may not survive a crash on some filesystems, silently losing state the caller believed committed ⁽ⁱ⁾
- **Remediation:** Detect and surface directory-fsync failures on platforms where durability is required, or document the durability boundary at the API level ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents-common/src/workstreams/ledger.rs:316`) ⁽ᵐ⁾:
```
if let Some(parent) = self.path.parent()
            && let Ok(dir) = std::fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
```

166. **Malformed inline tool-call XML silently discarded** — Malformed tool-call blocks are dropped with no logging, so a model emitting broken JSON tool calls will silently skip dispatch ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/llm/helpers/mod.rs:121
- **Business impact:** Agent tool invocations can be silently lost with no diagnostic trail, making failures difficult to reproduce and debug ⁽ⁱ⁾
- **Remediation:** Log or surface parse failures for tool_call blocks instead of swallowing them silently ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/llm/helpers/mod.rs:121`) ⁽ᵐ⁾:
```
.filter_map(|cap| serde_json::from_str(cap.get(1)?.as_str().trim()).ok())
```

167. **Resource gauge ceilings hardcoded rather than configurable** — Memory and disk gauge ceilings are compile-time constants, so deployments with different resource limits will show misleading gauges ⁽ⁱ⁾
- **Component:** crates/trusty-mpm/src/tui/health/screen.rs:384
- **Business impact:** Health TUI can misrepresent utilisation on larger or smaller hosts, reducing the operator's ability to trust the display ⁽ⁱ⁾
- **Remediation:** Source the gauge ceilings from configuration or reported daemon limits rather than hardcoded constants ⁽ⁱ⁾
- **Evidence** (`crates/trusty-mpm/src/tui/health/screen.rs:384`) ⁽ᵐ⁾:
```
const MEM_CEILING_MB: u64 = 8 * 1024;
```

168. **Static credential label read from mutable environment variable** — The API base URL is taken directly from an environment variable with no validation, allowing redirection of authenticated requests to an arbitrary host ⁽ⁱ⁾
- **Component:** crates/trusty-agents/src/llm/helpers/mod.rs:95
- **Business impact:** A compromised or misconfigured environment could redirect credentialed traffic to an attacker-controlled endpoint, leaking API keys ⁽ⁱ⁾
- **Remediation:** Validate the base URL against an allow-list or restrict overrides to trusted values ⁽ⁱ⁾
- **Evidence** (`crates/trusty-agents/src/llm/helpers/mod.rs:95`) ⁽ᵐ⁾:
```
let base_url = std::env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
```

### 5.3 GREEN / POSITIVE Findings (topic list ONLY — no elaboration)

- Read-only snapshot writes rejected at the store boundary — `crates/trusty-common/src/memory_core/store/hnsw_store.rs:120`

- Structured error enum distinguishes redb, codec, and UUID failures — `crates/trusty-common/src/memory_core/store/hnsw_store.rs:85`

- Credential resolution centralized as data on Command rather than per-front-end logic — `crates/trusty-audit/src/session.rs:423`

- Fail-open catch-up degrades cleanly when memory daemon unreachable — `crates/trusty-code/src/catchup.rs:39`

- Test isolation via passed-in URL removes shared mutable global state — `crates/trusty-code/src/catchup.rs:67`

- Credential resolver avoids stdin and uses /dev/tty with background-group refusal — `crates/trusty-audit/src/cli/credential.rs:233`

- Prompted credential persisted owner-only via atomic private write — `crates/trusty-audit/src/cli/credential.rs:531`

- Timeout thresholds centralized with env overrides and pure, tested parsing — `crates/trusty-common/src/memory_core/timeouts.rs:256`

- LLM HTTP client enforces connect and read timeouts to avoid indefinite hangs — `crates/trusty-agents/src/llm/http/mod.rs:85`

- Website pins transitive dependency overrides for known-vulnerable packages — `website/package.json:58`

- Project registry read/write failures degrade to logged 500 without panic — `crates/trusty-mpm/src/daemon/managed_routes/project_registry_routes.rs:124`

- tc-services declares versioned internal dependencies with explicit TLS stack rationale — `crates/tc-services/Cargo.toml:21`

- Atomic append-message write transaction closes concurrency race — `crates/trusty-common/src/memory_core/store/chat_sessions/store.rs:352`

- Bounded dispatcher survives panicking jobs without retiring workers — `crates/trusty-common/src/index_dispatch.rs:258`

- Ticket-leak redaction is deliberately designed and tested — `crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:127`

- Model-supplied path confinement enforced with explicit prefix check — `crates/trusty-agents/src/tools/okg/mod.rs:135`

- Anthropic response translation covers native usage mapping with focused unit tests — `crates/trusty-common/src/inference/providers/anthropic/response.rs:232`

- Thoroughly unit-tested retry/backoff logic covering budgets and edge cases — `crates/trusty-git-analytics/src/collect/jira/retry.rs:394`

- Constant-time comparison correctly used for shared-secret authorization — `crates/trusty-agents/Cargo.toml:136`

- Default-deny allowlist with hard sensitive-path denylist protects secret-bearing directories — `crates/trusty-search/src/allowlist/mod.rs:68`

- Panic-to-error boundary and per-path serialization guard for concurrent redb opens — `crates/trusty-search/src/core/corpus/open_guard.rs:417`

- tokio full-feature dependency pulls entire async surface into agent crate — `crates/trusty-agents/Cargo.toml:86`

- SQLite bookkeeping uses parameterized queries and idempotent INSERT OR REPLACE — `crates/trusty-git-analytics/src/core/db/collection_runs.rs:62`

- Language-agnostic Symbol extraction with clear kind mapping — `crates/trusty-common/src/symgraph/symbol.rs:106`

- HTTP-server deps gated behind optional feature flag — `crates/trusty-analyze/Cargo.toml:152`

- Strict deny_unknown_fields schema with explicit version gating prevents silent prompt corruption — `crates/trusty-mpm/src/core/instruction_package.rs:96`

- Credential-in-package invariant enforced by the type system — `crates/trusty-audit/src/config.rs:12`

- Injectable scan core enables hermetic filesystem tests — `crates/trusty-code/src/fs_browse/roster.rs:204`

- Evidence verification guardrail rejects unverifiable LLM findings fail-closed — `crates/trusty-review/src/report/investigate/verify.rs:135`

- Secret type is neither serialisable, cloneable, nor printable by construction — `crates/trusty-common/src/credentials/authority.rs:17`

- Env-var-driven scalability limits for warm-boot and residency — `crates/trusty-search/src/service/lazy_loader/mod.rs:6`

- Concurrency race conditions handled via token-guarded reaps and post-gate re-checks — `crates/trusty-search/src/service/lazy_loader/mod.rs:274`

- Corrupt-value handling for indexed_root fails closed instead of silently skipping the root-move gate — `crates/trusty-search/src/core/corpus/meta_ops.rs:205`

- Bulk-copy uses all-or-nothing atomic transaction to prevent partial-promotion data loss — `crates/trusty-search/src/core/corpus/meta_ops.rs:430`

- Injected env lookup removes ambient global-state coupling from auth resolution — `crates/trusty-git-analytics/src/collect/bitbucket/client.rs:78`

- Credential-bearing auth enum deliberately omits Debug derive — `crates/trusty-git-analytics/src/collect/bitbucket/client.rs:52`

- SSRF loopback guard on reverse-proxy upstream URLs — `crates/trusty-console/src/proxy/routes.rs:81`

- Stale-write guard prevents older snapshots downgrading PR state — `crates/trusty-git-analytics/src/collect/bitbucket/client.rs:329`

- Panic hook installed idempotently and preserves the previous hook — `crates/trusty-common/src/panic_hook.rs:43`

- KG API client forwards bearer token consistently to sidecar — `crates/trusty-agents/ui/src/lib/kg.ts:31`

- Explicit per-item error isolation in bulk vector upsert — `crates/trusty-search/src/core/store/usearch_impl.rs:464`

- RAII write-lock guard guarantees lock release on all SSE stream exit paths — `crates/trusty-mpm/src/daemon/api/control_routes.rs:60`

- Bounded, drop-oldest log capture buffer prevents unbounded memory growth — `crates/trusty-git-analytics/src/commands/tui/capture.rs:45`

- Token exchange parse failure redacts body before entering error chain — `crates/trusty-gworkspace/src/api/auth/oauth/flow/mod.rs:459`

- Fail-open per-file/per-commit error handling isolates enrichment failures from the reviewer — `crates/trusty-review/src/integrations/context/pr_history.rs:563`

- PATCH client surfaces server error messages with a JSON fallback for non-JSON bodies — `crates/trusty-agents/ui/src/lib/agentConfig.ts:105`

- Per-palace write serialization prevents concurrent-write data loss — `crates/trusty-common/src/memory_core/retrieval/handle.rs:139`

- KG tool handlers validate required arguments with explicit errors — `crates/trusty-memory/src/tools/kg_ops.rs:30`

- Fail-closed three-state HMAC verdict unifies webhook signature checking — `crates/trusty-common/src/webhook_hmac.rs:95`

- Constant-time HMAC comparison via Mac::verify_slice — `crates/trusty-common/src/webhook_hmac.rs:108`

- Corpus read fault surfaced as typed 503 instead of silent empty results — `crates/trusty-search/src/core/indexer/corpus_fault.rs:36`

- Bounded chat history prevents unbounded per-chat memory growth — `crates/trusty-mpm/src/telegram/mod.rs:537`

- Zero-feature test target guard prevents silently passing over uncompiled modules — `crates/trusty-common/src/lib.rs:56`

- MCP-initiated session spawn gate defaults to disabled (safe by default) — `crates/trusty-mpm/src/core/trusty_tools_config.rs:196`

- Reader task death drains pending requests and signals unhealthy — `crates/trusty-common/src/embedder_client/stdio.rs:382`

- Fail-closed rustdoc loader rejects unrecognised schema versions — `scripts/lib/rustdoc_walk.py:446`

- gh enforcement refuses to mutate shared config store without isolated config dir — `crates/trusty-mpm/src/core/gh_account_enforce.rs:277`

- Bounded reconnect with terminal failure event prevents silent stall — `crates/trusty-code/src/tui_client/engine_state.rs:412`

- Shared ETXTBSY retry policy with single classification function and contract tests — `crates/trusty-common/src/spawn_retry.rs:65`

- Mutex poisoning uniformly recovered rather than propagated — `crates/trusty-code/src/tui_client/engine_state.rs:88`

- Fail-open discovery guards prevent silently truncated repository lists — `crates/trusty-audit/src/discover.rs:148`

- Credential probe never retains the token value past its existence check — `crates/trusty-audit/src/discover.rs:178`

- Path-traversal guard at single choke point for agent name resolution — `crates/trusty-agents/src/agents/loader.rs:37`

- Fail-closed index budget with malformed-value defaulting — `crates/trusty-search/src/service/index_budget.rs:213`

- Robust data-dir sanitization against root-scatter and CWD-relative paths — `crates/trusty-common/src/data_dir.rs:84`

- Exponential-backoff retry with capped attempts for transient GitHub errors — `crates/trusty-git-analytics/src/collect/github/retry.rs:46`

- Typed error load path proven not to panic on corrupt ledger data — `crates/trusty-agents-common/src/workstreams/ledger.rs:534`

- Publish guardrail fails closed on unverifiable version-live status — `scripts/preflight-publish.sh:55`

- Concurrent-create test proves in-process write serialization — `crates/trusty-agents-common/src/workstreams/ledger.rs:499`

- Pure line-builder helpers designed for terminal-free unit testing — `crates/trusty-mpm/src/tui/health/screen.rs:256`

- Saturating selection index prevents out-of-bounds state — `crates/trusty-mpm/src/tui/health/screen.rs:131`

- Shared 3-tier credential resolver used instead of raw env reads — `crates/trusty-agents/src/llm/helpers/mod.rs:83`

- Well-maintained async-openai client dependency for OpenRouter — `crates/trusty-agents/src/llm/helpers/mod.rs:97`

## Code Quality & Architecture

The estate is a 30-crate Cargo workspace with 48 internal dependency edges and no dependency cycles — a clean acyclic topology that is a real structural strength. It is anchored by a shared core, trusty-common (16 dependents) and trusty-mcp (11 dependents), so defects or churn in either propagate widely; the remaining crates are largely leaves depended on by one member. Against that, the data shows a broad maintainability backlog: numerous oversized impl blocks flagged for splitting across trusty-common, trusty-search, trusty-agents and trusty-audit, and extract-method targets including a monolithic MCP dispatch function measured at cyclomatic complexity 118 that routes 30+ tools through one match arm. Total LoC is roughly 1.56M across Rust, HTML, Shell and Svelte. The topology is sound; the concern is concentration of logic in a few oversized, load-bearing surfaces. ⁽ⁱ⁾

| Application | LoC | Primary tech | Complexity profile | Maintainability findings |
|---|---|---|---|---|
| 00-local-trusty-tools | 1557669 ⁽ᵐ⁾ | Rust, HTML, Shell ⁽ᵐ⁾ | A: simple (0-5): 58812 (89%); B: moderate (6-10): 4535 (7%); C: elevated (11-15): 1568 (2%); D: high (16-20): 638 (1%); F: very high (>20): 678 (1%) ⁽ᵐ⁾ | 20 ⁽ᵐ⁾ |

30 crates, 48 internal dependency edge(s). No dependency cycles. Most depended on: trusty-common (16), trusty-mcp (11), trusty-progress (4), trusty-agents-common (3), trusty-gworkspace (2). ⁽ᵐ⁾

| Crate | Direct internal deps | Depended on by |
|---|---|---|
| trusty-common | trusty-mcp | 16 |
| trusty-mcp | — | 11 |
| trusty-progress | — | 4 |
| trusty-agents-common | — | 3 |
| trusty-gworkspace | trusty-common, trusty-mcp | 2 |
| trusty-cto-db | — | 1 |
| trusty-tui | — | 1 |
| trusty-embedderd | trusty-common | 1 |
| trusty-embedderd-py | trusty-common | 1 |
| trusty-kb | trusty-mcp | 1 |
| trusty-installer | trusty-common, trusty-progress | 1 |
| trusty-memory | trusty-common, trusty-mcp | 1 |
| trusty-review | trusty-common, trusty-mcp | 1 |
| trusty-audit | trusty-common, trusty-installer, trusty-progress | 1 |
| trusty-code | trusty-agents-common, trusty-common, trusty-mcp, trusty-tui | 1 |
| trusty-search | trusty-common, trusty-embedderd, trusty-embedderd-py, trusty-mcp | 1 |
| trusty-agents | trusty-agents-common, trusty-code, trusty-common, trusty-gworkspace, trusty-kb, trusty-mcp, trusty-memory, trusty-search | 1 |
| trusty-agents-ui | — | 0 |
| trusty-code-gui | — | 0 |
| trusty-mpm-gui | — | 0 |
| trusty-publish-guard | — | 0 |
| trusty-agents-local | trusty-agents | 0 |
| trusty-audit-ui | trusty-audit | 0 |
| trusty-console | trusty-common | 0 |
| trusty-sld-lint | trusty-common | 0 |
| tc-services | trusty-cto-db, trusty-gworkspace | 0 |
| tga | trusty-common, trusty-progress | 0 |
| trusty-channels | trusty-common, trusty-mcp | 0 |
| trusty-analyze | trusty-common, trusty-mcp, trusty-review | 0 |
| trusty-mpm | trusty-agents-common, trusty-common, trusty-mcp, trusty-progress | 0 |

## Security Posture

*This table counts the RED and AMBER findings the repo-evidence investigation
raised in its "authentication & secrets" dimension — an LLM reading the
selected source files, with each counted finding's evidence quote mechanically
verified against the file it cites. That verification claim covers the RED and
AMBER findings this table counts; a GREEN clean signal elsewhere in this report
carries a `file:line` only when a verified quote backs it, and none otherwise.
Its scope is code hygiene around
credentials, tokens, and authentication paths in the files that were read. It
is not a SAST scan, not a dependency/CVE scan, and not a secrets scan of the
whole tree; the share of files read is stated under Investigation Coverage.
Read a low count as a small sample, not as a clean bill of health.*

| Application | Dimension | RED/AMBER findings |
|---|---|---|
| 00-local-trusty-tools | authentication & secrets ⁽ᵐ⁾ | 33 ⁽ᵐ⁾ |

Clean signals found in this dimension: Credential resolution centralized as data on Command rather than per-front-end logic (`crates/trusty-audit/src/session.rs:423`); Credential resolver avoids stdin and uses /dev/tty with background-group refusal (`crates/trusty-audit/src/cli/credential.rs:233`); Prompted credential persisted owner-only via atomic private write (`crates/trusty-audit/src/cli/credential.rs:531`); Ticket-leak redaction is deliberately designed and tested (`crates/trusty-agents/ui/src-tauri/src/sse_bridge.rs:127`); Model-supplied path confinement enforced with explicit prefix check (`crates/trusty-agents/src/tools/okg/mod.rs:135`); Constant-time comparison correctly used for shared-secret authorization (`crates/trusty-agents/Cargo.toml:136`); Default-deny allowlist with hard sensitive-path denylist protects secret-bearing directories (`crates/trusty-search/src/allowlist/mod.rs:68`); Credential-in-package invariant enforced by the type system (`crates/trusty-audit/src/config.rs:12`); Secret type is neither serialisable, cloneable, nor printable by construction (`crates/trusty-common/src/credentials/authority.rs:17`); Injected env lookup removes ambient global-state coupling from auth resolution (`crates/trusty-git-analytics/src/collect/bitbucket/client.rs:78`); Credential-bearing auth enum deliberately omits Debug derive (`crates/trusty-git-analytics/src/collect/bitbucket/client.rs:52`); SSRF loopback guard on reverse-proxy upstream URLs (`crates/trusty-console/src/proxy/routes.rs:81`); KG API client forwards bearer token consistently to sidecar (`crates/trusty-agents/ui/src/lib/kg.ts:31`); Token exchange parse failure redacts body before entering error chain (`crates/trusty-gworkspace/src/api/auth/oauth/flow/mod.rs:459`); Fail-closed three-state HMAC verdict unifies webhook signature checking (`crates/trusty-common/src/webhook_hmac.rs:95`); Constant-time HMAC comparison via Mac::verify_slice (`crates/trusty-common/src/webhook_hmac.rs:108`); MCP-initiated session spawn gate defaults to disabled (safe by default) (`crates/trusty-mpm/src/core/trusty_tools_config.rs:196`); gh enforcement refuses to mutate shared config store without isolated config dir (`crates/trusty-mpm/src/core/gh_account_enforce.rs:277`); Credential probe never retains the token value past its existence check (`crates/trusty-audit/src/discover.rs:178`); Path-traversal guard at single choke point for agent name resolution (`crates/trusty-agents/src/agents/loader.rs:37`); Shared 3-tier credential resolver used instead of raw env reads (`crates/trusty-agents/src/llm/helpers/mod.rs:83`). ⁽ᵐ⁾

## Performance & Scalability

No performance or scalability assessment was made for this report. This pipeline has no performance data source: it collects no load-test results, latency/throughput measurements, or capacity/scaling headroom data (DOC-67 §3). A performance assessment would require load tests run under representative traffic, latency and throughput percentiles, and capacity data showing how the system behaves as load and data volume grow — none of which was collected or assessed here. The repo-evidence investigation did raise scalability findings — sequential collection, connection and queue limits, unbounded growth — and they are listed by severity in section 5 under the scalability dimension. They are code-reading judgements about how the system is built to scale, not measurements of how it does.

## 6. Risk Registers

_No data available — see Gaps & Caveats._

## 7. Graph-Ready Data Appendix

<!-- dataset: health_factors_by_app | chart: radar | x: factor | y: score, group: application -->

<!-- dataset: tqi_benchmark_position | chart: bar | x: application | y: quartile_rank -->

<!-- dataset: violations_by_domain | chart: stacked-bar | x: application | y: violation_count, group: domain -->

<!-- dataset: cve_by_component_severity | chart: heatmap | x: component | y: severity -->

<!-- dataset: license_risk_tiers | chart: bar | x: license | y: component_count -->

<!-- dataset: cloud_maturity_by_tech | chart: stacked-bar | x: technology | y: roadblock_count, group: application -->

<!-- dataset: violations_by_horizon | chart: bar | x: horizon | y: violation_count, group: application -->

<!-- dataset: remediation_cost_by_tier | chart: bar | x: tier | y: cost, group: application -->

<!-- dataset: loc_by_technology | chart: stacked-bar | x: application | y: loc, group: technology -->
| Application | Technology | LoC | % of total |
|---|---|---|---|
| 00-local-trusty-tools | Rust | 1,412,769 ⁽ᵐ⁾ | 91% ⁽ᵐ⁾ |
| 00-local-trusty-tools | HTML | 33,376 ⁽ᵐ⁾ | 2% ⁽ᵐ⁾ |
| 00-local-trusty-tools | Shell | 31,635 ⁽ᵐ⁾ | 2% ⁽ᵐ⁾ |
| 00-local-trusty-tools | Svelte | 28,099 ⁽ᵐ⁾ | 2% ⁽ᵐ⁾ |
| 00-local-trusty-tools | TypeScript | 24,181 ⁽ᵐ⁾ | 2% ⁽ᵐ⁾ |
| 00-local-trusty-tools | JavaScript | 17,101 ⁽ᵐ⁾ | 1% ⁽ᵐ⁾ |
| 00-local-trusty-tools | Python | 5,966 ⁽ᵐ⁾ | 0% ⁽ᵐ⁾ |
| 00-local-trusty-tools | CSS | 3,434 ⁽ᵐ⁾ | 0% ⁽ᵐ⁾ |
| 00-local-trusty-tools | SQL | 843 ⁽ᵐ⁾ | 0% ⁽ᵐ⁾ |
| 00-local-trusty-tools | Ruby | 265 ⁽ᵐ⁾ | 0% ⁽ᵐ⁾ |

```mermaid
%% stacked-bar approximated as layered (overlaid) bars — Mermaid has no native stacking
xychart-beta
    title "loc by technology"
    x-axis ["00-local-trusty-tools"]
    y-axis "loc"
    bar [1412769]
    bar [33376]
    bar [31635]
    bar [28099]
    bar [24181]
    bar [17101]
    bar [5966]
    bar [3434]
```
_Layered series (front-to-back): Rust, HTML, Shell, Svelte, TypeScript, JavaScript, Python, CSS._
_… and 2 more series omitted from the chart; see table above._

<!-- dataset: complexity_distribution | chart: bar | x: complexity_bucket | y: count, group: application -->
| Application | Complexity bucket | Count | % of total complexity |
|---|---|---|---|
| 00-local-trusty-tools | A: simple (0-5) | 58812 ⁽ᵈ⁾ | 89% ⁽ᵈ⁾ |
| 00-local-trusty-tools | B: moderate (6-10) | 4535 ⁽ᵈ⁾ | 7% ⁽ᵈ⁾ |
| 00-local-trusty-tools | C: elevated (11-15) | 1568 ⁽ᵈ⁾ | 2% ⁽ᵈ⁾ |
| 00-local-trusty-tools | D: high (16-20) | 638 ⁽ᵈ⁾ | 1% ⁽ᵈ⁾ |
| 00-local-trusty-tools | F: very high (>20) | 678 ⁽ᵈ⁾ | 1% ⁽ᵈ⁾ |

```mermaid
xychart-beta
    title "complexity distribution"
    x-axis ["A: simple (0-5)", "B: moderate (6-10)", "C: elevated (11-15)", "D: high (16-20)", "F: very high (>20)"]
    y-axis "count"
    bar [58812, 4535, 1568, 638, 678]
```

## 8. Ticketing & Delivery Traceability

2397 of 2972 commit(s) reference 3160 of 3160 synced board item(s) across github. ⁽ᵐ⁾

## 9. Gaps & Caveats

- Collection stage `jira sync` did not complete (no JIRA project scope: pass --project <KEY> or set jira.project_key in config.yaml) — the data it produces is not assessed in this report. Read the affected sections as unassessed, not as a clean result.
- Data handling: a formal data-retention attestation for this run is pending (#5218) and is not asserted here. tga's database records commit, pull-request, and ticket metadata; it stores no file content, diffs, patches, hunks, or blobs. Free-text fields it does store — commit messages, pull-request and ticket titles — are retained verbatim and carry whatever their authors wrote into them.
- Investigation batches (00-local-trusty-tools): 1 of 31 batch(es) were sent but their analysis truncated or failed (unparseable response) — batch(es) 9. The 4 file(s) they carried were read and produced no finding: crates/trusty-common/src/memory_core/analytics.rs, crates/trusty-review/src/pipeline/runner_mapreduce.rs, crates/trusty-search/src/core/corpus/corpus_ops.rs, crates/trusty-mpm/src/core/standalone/run.rs. Read their absence as unassessed, not as clean.
- Trace verdicts (00-local-trusty-tools): 10 of 15 traced finding(s) could not be checked against the symbol graph (The trace shows only the constant declaration `SHRINK_GUARD_RATIO_DIVISOR: usize = 2` with no implementation of the shrink guard logic, reindex procedure, or snapshot persistence mechanism that the finding describes.; The trace shows only the constant declaration and unrelated constants, with no code demonstrating how MIN_BYTES_PER_DIM_PER_VECTOR is used to validate or guard against truncated snapshot files before calling view().; The trace shows only the HnswStore struct definition and error conversion implementations, but does not include the actual body of the `open` method at line 356 where the finding's claim about per-open rebuild logic would be verified or contradicted.). Read those findings as unverified, not as disproved — the trace pass neither supported nor contradicted them.

Data gaps: 13 unpopulated fields/sections — Client; Analyst (this instance); Risk tier (native); Normalized score (0-100); Band; Health-Factor Scores; Benchmark Position; 6.1 Open-Source / CVE Exposure; 6.2 License / IP Risk; 6.3 Obsolescence; 6.4 Cloud Readiness Blockers; 6.5 Technical-Debt / Remediation Economics; 6. Risk Registers.



## Synthesis Status

- section 5.2, AMBER finding 103: the model's security summary was withheld — a beyond-the-host reachability claim about 'Admin stop endpoint trusts every caller with no authentication' could not be corrected safely — the report's own evidence scopes that surface to localhost
- section 5.1, RED finding 3: the model's business impact was withheld — a beyond-the-host reachability claim about 'claude_cmd override lets callers designate an arbitrary executable path' could not be corrected safely — the report's own evidence scopes that surface to localhost


## Dependency Inventory

_Parsed deterministically from manifests + lockfiles ⁽ᵐ⁾; staleness judgements, where present, are inferred ⁽ⁱ⁾._

### 00-local-trusty-tools

| Package | Ecosystem | Declared | Locked |
|---|---|---|---|
| anyhow | cargo | 1 | 1.0.102 |
| arc-swap | cargo | 1 | 1.9.2 |
| async-trait | cargo | 0.1 | 0.1.89 |
| aws-config | cargo | 1.8 | 1.8.17 |
| aws-sdk-bedrockruntime | cargo | 1.131 | 1.132.0 |
| aws-smithy-types | cargo | 1.4 | 1.4.8 |
| aws-types | cargo | 1 | 1.3.16 |
| axum | cargo | 0.8 | 0.8.9 |
| base64 | cargo | 0.22 | 0.22.1 |
| calamine | cargo | 0.36 | 0.36.0 |
| candle-core | cargo | 0.8 | 0.8.4 |
| candle-nn | cargo | 0.8 | 0.8.4 |
| candle-transformers | cargo | 0.8 | 0.8.4 |
| chrono | cargo | 0.4 | 0.4.44 |
| chrono-tz | cargo | 0.9 | 0.9.0 |
| clap | cargo | 4 | 4.6.1 |
| clap_complete | cargo | 4 | 4.6.5 |
| colored | cargo | 2 | 2.2.0 |
| comrak | cargo | 0.52 | 0.52.0 |
| criterion | cargo | 0.5 | 0.5.1 |
| crossterm | cargo | 0.28 | 0.28.1 |
| dashmap | cargo | 6 | 6.2.1 |
| dirs | cargo | 5 | 5.0.1 |
| dotenvy | cargo | 0.15 | 0.15.7 |
| fastembed | cargo | 5 | 5.13.4 |
| fd-lock | cargo | 4 | 4.0.4 |
| flate2 | cargo | 1 | 1.1.9 |
| futures | cargo | 0.3 | 0.3.32 |
| futures-util | cargo | 0.3 | 0.3.32 |
| git2 | cargo | 0.20 | 0.20.4 |
| … and 99 more | | | |



## Investigation Coverage

_What the repo-evidence pass actually inspected ⁽ᵐ⁾._

### 00-local-trusty-tools

- investigation: available
- files examined: 140 of 6683 tracked (2.1% coverage, 6543 skipped); 2457600 bytes sent (budget 240/2457600)
- dimensions covered: authentication & secrets, dependencies, state management, error handling, scalability, test coverage
- dimensions NOT investigated: none
- evidence discovery: 140 of 140 examined file(s) came from manifest-declared evidence queries
- attributed-only selection: 140 of 240 budgeted file(s) carried search or complexity evidence; the remaining 100 were left unread rather than filled by path-name heuristics
  - authentication & secrets: 27 file(s) examined — e.g. crates/trusty-audit/src/session.rs (trusty-search hit for "credential handling: api key, token or secret read from configuration or environment" (score 0.01, line 1714))
  - dependencies: 21 file(s) examined — e.g. Cargo.toml (build manifest present in the repository (Cargo.toml))
  - state management: 27 file(s) examined — e.g. crates/trusty-audit/src/grounding/evidence.rs (trusty-search hit for "cache invalidation and consistency after a crash or restart" (score 0.01, line 151))
  - error handling: 18 file(s) examined — e.g. crates/trusty-code/src/catchup.rs (trusty-search hit for "panic, unwrap or process exit on a failure path" (score 0.01, line 208))
  - scalability: 22 file(s) examined — e.g. crates/trusty-memory/src/commands/kg_rebuild.rs (trusty-search hit for "query inside a loop, unbounded collection growth or full scan" (score 0.01, line 488))
  - test coverage: 20 file(s) examined — e.g. crates/trusty-analyze/src/core/mod.rs (trusty-search hit for "integration test exercising an external boundary" (score 0.01, line 72))
- investigation: 12 finding(s) rejected (unverifiable evidence)
- verified findings: 226 (153 RED/AMBER evidence-backed, 73 clean signals)
- section 5.2 reconciliation: 226 investigation-verified finding(s) + 20 finding(s) trusty-analyze measured mechanically = 246 entries rendered (5 RED, 168 AMBER, 73 GREEN)
- traces assembled: 9 of 15 candidate findings (6 no-trace)
- trace verdicts: 5 confirmed, 0 cleared, 10 unverifiable of 15 traced
  - judged by: anthropic/claude-haiku-4.5
  - unverifiable: Documented persistent data-loss window in HNSW snapshot persistence (issue #3970) — The trace shows only the constant declaration `SHRINK_GUARD_RATIO_DIVISOR: usize = 2` with no implementation of the shrink guard logic, reindex procedure, or snapshot persistence mechanism that the finding describes.
  - unverifiable: usearch view() can SIGSEGV the entire process on a truncated snapshot file — The trace shows only the constant declaration and unrelated constants, with no code demonstrating how MIN_BYTES_PER_DIM_PER_VECTOR is used to validate or guard against truncated snapshot files before calling view().
  - unverifiable: hnsw_rs cannot remove points, leaving stale shadow vectors in the graph — The trace shows only the HnswStore struct definition and error conversion implementations, but does not include the actual body of the `open` method at line 356 where the finding's claim about per-open rebuild logic would be verified or contradicted.
  - unverifiable: Multi-transaction compact_orphans creates a reachable inconsistent state — The trace shows `high_water` is a function declared at line 235 that reads from the vectors and keys tables, but does not show the `compact_orphans` function or the three-transaction deletion sequence described in the finding's evidence quote.
  - unverifiable: Silent aliasing bug history indicates fragile state invariants — no trace: symbol AliasAudit resolved to crates/trusty-common/src/memory_core/retrieval/embed_repair.rs — ambiguous name
  - unverifiable: MCP dispatch function is a monolithic 118-cyclomatic switch — no trace: symbol dispatch resolved to crates/trusty-memory/src/transport/rpc.rs — ambiguous name
  - unverifiable: MCP tool dispatch has only indirect/integration test coverage — no trace: symbol handle_tool_call resolved to crates/trusty-channels/src/slack/server.rs — ambiguous name
  - unverifiable: run_stdio and transition tools marked untested or manually tested only — no trace: symbol run_stdio resolved to crates/trusty-channels/src/slack/server.rs — ambiguous name
  - unverifiable: Time-boxed dependency vulnerability exception cannot self-expire — no trace: no item declaration found at deny.toml:231
  - unverifiable: Yanked and unsound advisories deliberately downgraded from deny — no trace: no item declaration found at deny.toml:211
- batches: 31 total, 30 succeeded, 1 truncated/failed
  - batch 9 of 31: sent but analysis truncated/failed (unparseable response) — files: crates/trusty-common/src/memory_core/analytics.rs, crates/trusty-review/src/pipeline/runner_mapreduce.rs, crates/trusty-search/src/core/corpus/corpus_ops.rs, crates/trusty-mpm/src/core/standalone/run.rs

---
*Generated by trusty-review report analysis — template report-technical-dd v0.1*
*Source: repository inspection (manifest: /Users/masa/dogfood-audit-rerun4/out/00-local-trusty-tools/manifest.toml)*
