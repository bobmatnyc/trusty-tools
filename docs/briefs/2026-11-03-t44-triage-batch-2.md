# T44 triage batch 2 (df-T2a, list-only)

Generated 2026-11-03 from `gh issue list --state open` (257 open) and Project #45 Status. No issue was closed, labeled, commented on or edited. Excluded: every issue in the three earlier t44 Won't Do lists, #8021, and all `type:epic` trackers.

## 1. Evidence-close candidates (fix on origin/main, rung allows close at Merged)

| Issue | Title | Fix | Rung | Note |
|---|---|---|---|---|
| [#10631](https://github.com/bobmatnyc/trusty-tools/issues/10631) | ci: "CI gate" reports success while the pre-publish shard aggregate fails on main | [#10710](https://github.com/bobmatnyc/trusty-tools/pull/10710) / [51ed3b4f30](https://github.com/bobmatnyc/trusty-tools/commit/51ed3b4f30) | CI | Project Status Merged |
| [#10450](https://github.com/bobmatnyc/trusty-tools/issues/10450) | feat(content): add neon-postgres framework skill (Neon, Vercel DB env vars, preview-branch DBs, | [#10452](https://github.com/bobmatnyc/trusty-tools/pull/10452) / [8850d8bea1](https://github.com/bobmatnyc/trusty-tools/commit/8850d8bea1) | 3 | content skill asset; Project Status Merged |
| [#10262](https://github.com/bobmatnyc/trusty-tools/issues/10262) | fix(trusty-mpm): fleet init refuses a dir over its own linked worktrees | [#10559](https://github.com/bobmatnyc/trusty-tools/pull/10559) / [c51a78e6aa](https://github.com/bobmatnyc/trusty-tools/commit/c51a78e6aa) | 3 | trusty-mpm only |
| [#10260](https://github.com/bobmatnyc/trusty-tools/issues/10260) | bug(trusty-mpm): build-lease census counts Linux threads as processes, refusing unleased builds | [#10561](https://github.com/bobmatnyc/trusty-tools/pull/10561) / [5faa55f828](https://github.com/bobmatnyc/trusty-tools/commit/5faa55f828) | 3 | closure condition 2 split to #10562 (already listed); comment says it stays open for a live check, which rung 3 does not require |
| [#10670](https://github.com/bobmatnyc/trusty-tools/issues/10670) | fix(trusty-mpm): answered dashboard decisions don't reach Architect | [#10681](https://github.com/bobmatnyc/trusty-tools/pull/10681) / [ab92f80039](https://github.com/bobmatnyc/trusty-tools/commit/ab92f80039) | 3 | trusty-mpm + python/trusty-architect |

Not listed though merged: #10619 (closure needs each hnsw test under 60 s on a CI run; no such run cited), #10042 and #10071 (reopened 2026-10-30 for daemon-restart live verification), #10083, #10120, #10584, #10588, #10600, #10614, #10662 (rung 4-5, close at On Prod).

## 2. Won't Do list 3 (parked P3/stale)

| Issue | Title | Age (d) | Last activity | Reason |
|---|---|---|---|---|
| [#6852](https://github.com/bobmatnyc/trusty-tools/issues/6852) | trusty-console UI: /ui/stream/list machine-wide virtualized event list (DOC-73 §5.1, slice 7) | 59 | 2026-10-29 | P3 console UI view from DOC-73 spec; no PR or owner since filing |
| [#6853](https://github.com/bobmatnyc/trusty-tools/issues/6853) | trusty-console UI: /ui/stream/tree session-agent-subagent-event tree with 2000-node/300ms measu | 59 | 2026-10-29 | P3 console UI view from DOC-73 spec; no PR or owner since filing |
| [#6855](https://github.com/bobmatnyc/trusty-tools/issues/6855) | trusty-console: /ui/object/<type>/<id> read-only object viewer for the nine DOC-73 §6 types, an | 59 | 2026-10-29 | P3 console UI view from DOC-73 spec; no PR or owner since filing |
| [#8343](https://github.com/bobmatnyc/trusty-tools/issues/8343) | worktrees: lease fixed-path pool trees through the WorktreeCreate hook, and reconcile trees tha | 43 | 2026-10-31 | P3 tech-debt; no branch or PR in 43 d |
| [#8344](https://github.com/bobmatnyc/trusty-tools/issues/8344) | build (Rust adapter): measured baseline, a job budget per admitted build, and dev-profile setti | 43 | 2026-10-31 | P3 performance; no branch or PR |
| [#9080](https://github.com/bobmatnyc/trusty-tools/issues/9080) | chore(trusty-mpm): add #[non_exhaustive] to public error enums for 2.0.0 | 32 | 2026-10-31 | P3 chore tied to 2.0.0 semver; owner can re-file when 2.0 is scheduled |
| [#9537](https://github.com/bobmatnyc/trusty-tools/issues/9537) | chore(trusty-common): remove the PreHandshake fallback once every daemon ships memory.protocol | 25 | 2026-10-31 | P3 cleanup waits on every daemon shipping the new handshake; no trigger date |
| [#10059](https://github.com/bobmatnyc/trusty-tools/issues/10059) | feat(release): manually triggered eval suite for bundled agents and skills, run once before eac | 15 | 2026-10-29 | P3 manual eval suite; no scheduled use |
| [#10067](https://github.com/bobmatnyc/trusty-tools/issues/10067) | chore(trusty-architect): ledger import and copied-prototype migration | 15 | 2026-10-29 | P3 migration of a copied prototype; no consumer |
| [#10488](https://github.com/bobmatnyc/trusty-tools/issues/10488) | Architect as a TUI | 5 | 2026-10-30 | P3 roadmap idea, no design or owner |
| [#10617](https://github.com/bobmatnyc/trusty-tools/issues/10617) | trusty-mpm: Chromium MCP browser-control adapter (build, from spike #10361) | 3 | 2026-10-31 | P3 adapter built from a spike; no demand shown |
| [#5418](https://github.com/bobmatnyc/trusty-tools/issues/5418) | feat(mpm): deploy the canonical agent corpus as Codex custom agents | 84 | 2026-09-27 | P2 backlog/dx; no activity in 37 d, 84 d old |
| [#5421](https://github.com/bobmatnyc/trusty-tools/issues/5421) | feat(mpm): deploy canonical skills in the Agent Skills standard | 84 | 2026-09-27 | P2 backlog/dx; no activity in 37 d, 84 d old |
| [#4315](https://github.com/bobmatnyc/trusty-tools/issues/4315) | feat(trusty-agents): per-connection health/probe API for memory, search, and OKG | 97 | 2026-10-07 | P2 feature; idle 27 d, 97 d old, no PR |
| [#129](https://github.com/bobmatnyc/trusty-tools/issues/129) | tracker(trusty-search): benchmark results across releases — regression testing | 162 | 2026-10-19 | tracker for benchmark runs; labeled release-cleanup; 162 d old |
| [#2873](https://github.com/bobmatnyc/trusty-tools/issues/2873) | chore(deps): glib < 0.20 via tauri Linux GTK stack (Dependabot alert 41, moderate, Linux-only) | 109 | 2026-10-19 | moderate Dependabot alert blocked on tauri upstream; covered by tracker #2872 |

## 3. P1 list (should be P1 now; none carries P1 today)

| Issue | Title | Evidence |
|---|---|---|
| [#10612](https://github.com/bobmatnyc/trusty-tools/issues/10612) | trusty-mpm: an Architect launch whose hook write fails runs without the exec-form pm-guard | security: Architect launch whose hook write fails runs without the exec-form pm-guard (guard bypass) |
| [#10664](https://github.com/bobmatnyc/trusty-tools/issues/10664) | Persisted indexes can hold mixed INT8 and fp32 vectors with nothing to detect them | silent corruption: persisted indexes can hold mixed INT8 and fp32 vectors with nothing to detect them; fix for the cause is [#10675](https://github.com/bobmatnyc/trusty-tools/pull/10675) but stored data is unchecked |
| [#10682](https://github.com/bobmatnyc/trusty-tools/issues/10682) | trusty-memory: hnsw_rs parallel insert can deadlock on reverse neighbour update | hang: hnsw_rs parallel insert can deadlock on reverse neighbour update |
| [#10666](https://github.com/bobmatnyc/trusty-tools/issues/10666) | 20 sessions relaunched blank at 05:37:41Z after the tmux server died | fleet loss: 20 sessions relaunched blank after the tmux server died (currently P2) |
| [#10528](https://github.com/bobmatnyc/trusty-tools/issues/10528) | bug(trusty-mpm): daemon restart ends claude in tmux sessions with exit 143 | daemon restart ends claude in tmux sessions with exit 143 (live work killed on restart) |
| [#6067](https://github.com/bobmatnyc/trusty-tools/issues/6067) | Remove RUSTSEC-2026-0258 advisory exception (manual expiry — review by 2026-09-18) | supply-chain gate: RUSTSEC-2026-0258 exception passed its agreed review date 2026-09-18 with no action |
| [#10436](https://github.com/bobmatnyc/trusty-tools/issues/10436) | fix(trusty-mpm): clippy nonminimal_bool on main in bin/tm commands; CI does not lint these targ | broken gate: clippy nonminimal_bool fails on main in bin/tm commands and CI does not lint those targets |
| [#10692](https://github.com/bobmatnyc/trusty-tools/issues/10692) | bug(trusty-search): a refused walk (file budget exceeded) marks a Ready index's lexical, semant | wrong data: a refused walk (file budget exceeded) marks a Ready index lexical stage ready |

Already P1 and open (not repeated here): #10724, #10723, #10722, #10721, #10487, #9057, #10643, #10607, #10586.

Counts: section 1 = 5, section 2 = 16, section 3 = 8; total 29.
