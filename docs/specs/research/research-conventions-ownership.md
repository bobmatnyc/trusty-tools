# Conventions and ownership for the memory-sync docs PR

Research date 2026-10-04, main at e3ff4c7358. Read-only; no cargo, no gh.

## 0. Surprises
- A worktree for this PR already exists: `.claude/worktrees/docs-memory-sync`, branch docs/memory-sync-specs at e3ff4c7358 (main tip), clean, no commits.
- `docs/trusty-memory/decisions/` is a crate-scoped ADR dir (one record, `0001-frontend-core-split.md`). `docs/adr/README.md:41-49` ("Hybrid scope rule") sends crate-specific decisions there. Decide whether the three ADRs are workspace-wide (`docs/adr/`) or crate-scoped (`docs/trusty-memory/decisions/`, independent numbering).
- The memory-sync work builds on `crates/trusty-common/src/memory_core/share/` (cross-machine export/import, #5902, `mod.rs:1`). Name it as prior art.

## 1. Spec conventions
- Specs live in `docs/specs/`, one file per spec: `DOC-NN-kebab-title.md` (e.g. `docs/specs/DOC-78-claude-code-mods-integration.md`). Catalog: `docs/specs/README.md:34` ("Spec catalog"); policy `docs/specs/README.md:17-26`.
- There is no docs/requirements/ directory. `docs/adr/README.md:5-6` calls Requirements "DOC-43, future". Product PRDs are in `docs/prd/` (`PRD-AGENTS-01-trusty-agents.md`, `PRD-SECRETS-01-console-secrets.md`). Crate-level PRD/ARCHITECTURE/COMPONENTS live in `docs/trusty-memory/spec/` (`PRD.md`, `ARCHITECTURE.md`). A requirements doc fits `docs/specs/` as its own DOC-NN or `docs/prd/PRD-MEMSYNC-01-...md`. Both are repo-reasonable; pick one.
- Header (DOC-38 §4.2, `docs/specs/spec-linked-documentation.md:471-491`): title `# DOC-N — <Title>`, then bold fields. Required: **Status** (Draft|Accepted|Superseded), **Subsystem**, **Owner**, **Last-updated** (ISO), **Spec ID** (`SPEC-{SUBSYS}-{NN}~draft`, range `… …` allowed). Optional: Epic, Builds on, Cross-ref. DOC-78 also adds Target release and a **DOC-N claim** line (`docs/specs/DOC-78-claude-code-mods-integration.md:1-11`).
- Anchors: each governed section heading carries `{#SPEC-X-NN~draft}` (§4.3, line 493). The `{rev}` stays `draft` until accepted (§4.4, line 515).
- Optional `spec_refs:` YAML frontmatter only for outbound links (§2.5). Frontmatter opts a spec into full sld-lint checks; without it the spec is grandfathered.
- Numbering (§4.1, line 446): scan-before-claim. The README "Next free" hint is a hint, not authority. Current hint: **DOC-79** (`docs/specs/README.md:109`). Claiming DOC-79 for the spec means: add a catalog row (§4.5, line 534; row format at `docs/specs/README.md:97`) AND bump the hint to DOC-80. `check_doc_numbers.sh` fails if the hinted number is claimed (`scripts/check_doc_numbers.sh:342-349`). A requirements doc as a second DOC needs DOC-80 and the hint moves to DOC-81. Branch docs/memory-sync-specs has no commits claiming either.
- Best copy: `docs/specs/DOC-78-claude-code-mods-integration.md` (2026-10-02, catalog row at README:97).

## 2. ADR conventions
- Template `docs/adr/template.md:1-40`: title `# NNNN. <Title>`; bullets Status, Date, Scope, Reversibility Cost, Decision Drivers, Supersedes/Superseded by; sections Context, Decision, Consequences, Related Decisions (vetted-against list with verdicts Consistent/Extends/Supersedes/Conflict).
- Status values: Proposed | Accepted | Rejected | Superseded by [link] | Amended by [link] (`docs/adr/README.md:57-76`). Use **Proposed** for drafts. For ADR >= 0014 a non-Proposed/Rejected ADR needs a non-empty Related Decisions section (`scripts/check_adr.sh:232-236`). Prudent to fill it anyway.
- Numbering `NNNN-kebab-title.md` (`docs/adr/README.md:51-56`). Highest on main: **0065** (`docs/adr/0065-trusty-events-process-placement.md`; INDEX rows 0064/0065 at `docs/adr/INDEX.md:77-78`). `git log --all -- 'docs/adr/006[6-9]*'` is empty and no branch name claims 0066+. Free: 0066, 0067, 0068. Re-check `git branch -r` just before pushing; ADR-0064 had five branches.
- INDEX.md: add one row per ADR, `| [NNNN](file) | Title | Proposed | one-line decision | Scope |`, and bump "Last updated" (line 3). check_adr enforces index parity (`docs/adr/README.md:85-87`).
- Crate-scoped alternative: `docs/trusty-memory/decisions/README.md` (hand-maintained list) with its own numbering from 0002.

## 3. Gates for a rung-1 docs PR
Rung 1 = doc gates only; no cargo (`CLAUDE.md` Rust Test Ladder; `docs/reference/ci-gates.md:155-185`).
- `bash scripts/check_sld.sh` (`scripts/check_sld.sh:1-30`): resolves declared refs; full §4 checks only on frontmatter-bearing specs; requires a catalog row for each spec (the `spec-catalog` check, per `.github/workflows/doc-numbers.yml:8-12`). Prefers an installed `sld-lint`, else `cargo run`; the cargo fallback is a build, so check PATH first.
- `bash scripts/check_adr.sh` (add `--self-test` is separate): numbering, status grammar, index parity, Related Decisions.
- `bash scripts/check_doc_numbers.sh`: no duplicate DOC-N/ADR number; filename number equals header label; catalog rows point at files; next-free hint is free. Triggered by `docs/specs/**` and `docs/adr/**` (`.github/workflows/doc-numbers.yml:23-33`). Allowlist `.doc-number-allowlist.tsv`.
- `bash scripts/check_doc_paths.sh` (`.github/workflows/doc-paths.yml`, no paths filter): every backtick-quoted `crates/`, `src/`, `scripts/`, `docs/`, `.github/` token in the scanned live Markdown must resolve in the checkout. Biggest risk for a design doc: do not backtick a planned or nonexistent path (e.g. a future crates/trusty-memory/src/sync/ module). Write it without backticks or as a non-path.
- `scripts/check_test_pointers.sh` and `scripts/check_line_cap.sh` read `.rs` (and `.swift` for line cap) only. No-ops for markdown (`scripts/check_test_pointers.sh:15-17`, `scripts/check_line_cap.sh:20-34`). Docs-only needs no changelog fragment (`CLAUDE.md` changelog section exempts docs-only).
- `docs/public-manifest.tsv` is an allowlist (`docs/public-manifest.tsv:1-12`): absent = not public, so new specs/ADRs need no entry. `scripts/check_public_docs.sh` only validates listed pages.
- Website: `Website content corpus` job runs on `docs/**` changes (`docs/reference/ci-gates.md:171-185`); nothing to register.
- Registration summary: spec -> catalog row + hint bump in `docs/specs/README.md`; ADR -> `docs/adr/INDEX.md` row. `docs/SUMMARY.md` links only the READMEs (lines 15-17), so no entry. `docs/reference/crate-map.md` and the manifest need nothing.
- Proposed one-shot check (no cargo): `bash scripts/check_adr.sh; bash scripts/check_doc_numbers.sh; bash scripts/check_doc_paths.sh; bash scripts/check_sld.sh`.

## 4. API/protocol spec homes today
- trusty-mpm daemon HTTP: code-generated OpenAPI 3.1 via utoipa, `crates/trusty-mpm/src/daemon/openapi.rs:1-12`. No checked-in openapi.json.
- trusty-memory tool surface: OpenRPC 1.3.2 description in `crates/trusty-memory/src/openrpc.rs:1-8` (`rpc.discover`); it is code-generated.
- JSON Schema: `content/instructions/instruction-package.schema.json` (instructions only).
- Prose protocol specs: `docs/specs/` (e.g. `SPEC-MCPSVC-01-trusty-mcp-service.md`, `trusty-memory-chat-session-manager.md`), crate dirs `docs/trusty-memory/spec/`, `docs/trusty-mpm/spec/`; ADR-0065 puts the bus wire contract in `trusty-common::control_bus` code.
- Candidate home for a "remote memory-store API spec": a DOC-NN in `docs/specs/` (behavior contract), cross-referencing the OpenRPC doc as the eventual machine-readable form. No docs/reference/api directory exists.

## 5. In-flight trusty-memory work (runtime owner, assigned by the Architect)
Issue numbers #9172/#9173/#9174 appear in no branch, commit message, or doc in this checkout (`git log --all --grep` empty; docs grep empty). M3 (short ids) and "f0 reclaim" by name: no branch. Evidence below is by file overlap. Treat #9172-#9174 as unlocated and confirm with the owner.
- **M1** `fix/m1-memory-spike` (worktree `agent-a4b4a98314c42ff50`, head f38b33594c, 4 commits, Refs #9140/#9141; unmerged). Touches `crates/trusty-memory/src/commands/palace_reclaim*.rs` (reclaim dry-run, `--apply` to dated trash, `--purge-trash` at 7 days: this is "f0 reclaim"), `commands/stop*.rs`, `commands/service.rs`, `service/recall_stream.rs`, `tools/recall_ops.rs`, `chat/tools.rs`, `console_metrics/`; `trusty-common` `memory_core/store/hnsw_store{,/replay,/exhaustive}.rs`, `store/palace_store.rs`, `registry`, `retrieval/handle.rs`, `analytics.rs`, `filter/secret/google_bare_id.rs`. Behaviors: recall-all skips empty palaces; palace location beats palace.json; shared recall log; deterministic HNSW replay.
- **M2** `fix/m2-memory-ranking` (worktree `agent-af8aae87a2218d23b`, head bcbb3e393b, Refs #9142/#9143/#8246; unmerged). Touches (on that branch, not on main) crates/trusty-memory/src/tools/recall_rank.rs, `recall_rulings.rs` (new), `recall_projection.rs`, `recall_ops.rs`, `service/core_recall.rs`, `service/core.rs`, `chat/{handler,tools}.rs`, `startup_tasks.rs`, `lib.rs`, `main.rs`, tests `recall_rulings_leg.rs`, `recall_temporal_rank.rs`. Behaviors: stale session snapshots demoted below current rulings; user-scope rulings reached from any project palace. This is the "superseded demotion"/ranking item.
- Integration branch `eval/m1-m2` (worktree `eval-m1-m2`) merges M1+M2 locally; not for PR.
- Dream work: `fix/8733-dream-stats-atomic-write` (unmerged-ish; `memory_core/dream/{config,mod}.rs`, `atomic_file.rs`). Dream code on main: `crates/trusty-common/src/memory_core/dream/` (`cycle.rs`, `dreamer.rs`, `fading.rs`, `kg_compact.rs`, `semantic.rs`, `concurrency.rs`), `memory_core/semantic_consolidation/`, `memory_core/maintenance_lease.rs`, `crates/trusty-memory/src/dream_scheduler.rs`, `tools/dream_ops.rs`. Recent: #8732 dream deletions journaled (`maintenance_log.rs`), #8733 maintenance election. The dreaming fixes #9172-#9174 almost certainly land here.
- Old PoC `origin/feat/dream-consolidation-poc` (2026-07-16) carries docs/specs/trusty-memory-dream-consolidation.md, not on main.
- Sync design should list as dependencies, not changes: recall ranking and rulings (`recall_rank.rs`, `recall_rulings.rs`), short-id handling (grep hits only `dream_scheduler.rs` and `commands/backfill_report`; no id-shortening module found), reclaim trash semantics (`palace_reclaim*.rs`), HNSW replay/palace_store, and the dream/maintenance modules above (a sync merge must respect `maintenance_lease` and journaled deletions). Sync must not alter `Drawer.id` semantics; `share/supersede.rs` already handles convergence.

## 6. trusty-common pieces (modules only)
- `memory_rpc` (`crates/trusty-common/src/memory_rpc.rs`, #6286: `call_memory_tool`, `call_memory_tool_at`; UDS framed JSON-RPC); `uds`, `daemon_addr`, `daemon_token` (0600 per-app local credential), `daemon_guard`.
- `memory_core::share` (export, import, record, supersede), `memory_core::{maintenance_lease, maintenance_log, registry, room_identity, wing_identity, content_hash, decay, store/*, filter/secret}`.
- `monitor::memory_client` (and `monitor::dashboard`, `memory_tui`), `credentials::{keyring_store (feature keyring-store), resolver, file_store, redact, authority, bounded_store}`, `webhook_hmac`, `http_client`, `atomic_file`, `file_lock`, `palace_id`/`palace_alias`/`palace_resolve`, `launchd_secrets`.
