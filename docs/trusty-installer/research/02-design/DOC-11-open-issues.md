# DOC-11 — Open Issues & Adversarial-Review Tracker

**Status:** Active — living tracker (iterate per item)
**Source:** Adversarial (devil's-advocate) review of DOC-0..DOC-10, 2026-06-09

## Purpose

This document tracks the open issues raised against the **Accepted** trusty-controller
design set (DOC-0 through DOC-10 + ADR-0006/0007/0008). It is **not** a design doc with
an Accepted status — it is an **active issue tracker**. Each item is iterated with the
owner; decisions are recorded inline in that item's `Decision` field as they are made.
When an item is resolved, its `Status` flips and the affected design doc(s)/ADR(s) are
updated **separately** (the edits land in those docs, not here — this tracker only records
that the follow-up is needed and, once done, that it is `✅ Resolved`).

## Status legend

- 🔴 **Open** — raised, not yet triaged or decided.
- 🟡 **Deciding** — under active discussion with the owner.
- 🟢 **Decided** — a decision is recorded; doc/ADR edit not yet landed.
- ⚪ **Deferred** — acknowledged, intentionally postponed (e.g. post-v1).
- ❌ **Won't-fix/Rejected** — reviewed and intentionally not actioned.
- ✅ **Resolved (doc updated)** — decided **and** the affected doc(s)/ADR(s) are updated.

## Summary

| ID | Sev | Title | Affected docs | Verified? | Status |
|---|---|---|---|---|---|
| C1 | CRITICAL | `id_from_path` does not canonicalize; ADR-0008 "collision-free, symlink-safe" guarantee is false | ADR-0008, DOC-3 §8 | ✅ code-verified | ✅ Resolved |
| C2 | CRITICAL | DOC-8 launch-hook precedent is misattributed; no claude-mpm startup hook verified to exist | DOC-8 §4.1/§4.3/Resolved-Decision-4 | ✅ code-verified | ✅ Resolved |
| C3 | CRITICAL | `verbs[]` presence independent of `contract_version` opens an unversioned breaking-change channel | DOC-1 D3, ADR-0007 §3, DOC-1 ledger | — | ✅ Resolved |
| C4 | CRITICAL | DOC-4 dependency de-duplication needs root-cause inference the controller can't do generically | DOC-4 §5.4/§2.3/§8.2 | — | ✅ Resolved |
| C5 | CRITICAL | DOC-9 "new versions take effect via connection-safe restart" assumes a `restart` verb DOC-6 says is net-new on every tool | DOC-9 §5.1, DOC-6 §2.1–2.4 | — | ✅ Resolved |
| M1 | MAJOR | "Zero tool-specific logic" is oversold (relocated, not eliminated) | spec §83; DOC-2 §6, DOC-5 §1/§2.2, DOC-6 §4, DOC-3 §7, DOC-7 | — | ✅ Resolved |
| M2 | MAJOR | `stack_version` tuple will rot (no test owner, no CI gate, embedded in the binary) | DOC-2 §4, Resolved-Q5 | — | ✅ Resolved |
| M3 | MAJOR | Cross-repo claude-mpm dependency is load-bearing in 4 places but never assessed as one systemic risk | DOC-6 §4, DOC-8 §4/§5, DOC-9 §3.4, DOC-10 §6 | — | ✅ Resolved |
| M4 | MAJOR | `config` verb is a breaking CLI change + a hole in the read-only guarantee | DOC-1 `config.data`, DOC-5 §1.2, DOC-6 §2.6, DOC-3 Q3 | ✅ code-verified | ✅ Resolved |
| M5 | MAJOR | clap `external_subcommand` passthrough collides with first-class subcommands + controller-as-member recursion | DOC-5 §6/§1.1 | — | ✅ Resolved |
| M6 | MAJOR | CLI-as-authority (D1) under-specified for "daemon up but wedged" | DOC-1 D1, `health.data` | — | ✅ Resolved |
| M7 | MAJOR | Concurrent "ensure every launch" race on a shared daemon | DOC-3 §4/§6/§8 | — | ✅ Resolved |
| M8 | MAJOR | Status-enum reconciliation is ambiguous | DOC-1 D4, DOC-4 §1.1/§4, DOC-3 §9 | — | ✅ Resolved |
| M9 | MAJOR | D8 secret redaction is unenforceable | DOC-1 D8, DOC-6 conformance clause 5 | — | ✅ Resolved |
| M10 | MAJOR | Timeouts (2s health / 10s doctor) false-fail cold / model-loading daemons | DOC-4 §1.3, Resolved-Q1 | — | ✅ Resolved |
| M11 | MAJOR | `state.toml` stack-version tracking can silently desync; UI shows stale "clean" | DOC-9 §4.4, Resolved-Q3, DOC-7 §2.1 | — | ✅ Resolved |
| M12 | MAJOR | No auto-rollback + non-transactional installs can wedge a partial tuple | DOC-9 §6/§3.6, Resolved-Q5 | — | ✅ Resolved |
| M13 | MAJOR | Self-upgrade abandons in-flight UI op state | DOC-9 §8, DOC-7 §8/§3.2 | — | ✅ Resolved |
| M14 | MAJOR | CI tests the wrong paths for the primary target | DOC-10 §4.1/§6b, DOC-8 §6, Resolved-Decision-5/6 | — | ✅ Resolved |
| M15 | MAJOR | Project-identity migration orphans existing index/palace state | DOC-6 §7, DOC-3 §8, ADR-0008 | — | ✅ Resolved |
| m1 | MINOR | Aggregate exit code undefined for fan-out `stack doctor` | DOC-1 D5, DOC-4 | — | ✅ Resolved |
| m2 | MINOR | `pending` is gameable (no stall detection) | DOC-3 §2, DOC-1 `doctor.data`, Resolved-Q5 | — | ✅ Resolved |
| m3 | MINOR | Project-scope `fail`/`down` exits 0, masking a real local outage | DOC-4 §2.2/§7, Resolved-Q3 | — | ✅ Resolved |
| m4 | MINOR | `scope` vs config-provenance `scope` overload is leaky + stringly-typed | DOC-1 D7 + `config.data` (`ConfigSource.scope: String`) | — | ✅ Resolved |
| m5 | MINOR | `enabled=false` vs the "no uninstall" non-goal is undefined for the ensure pass | DOC-2 (`enabled`), DOC-3 §4, spec §62 | — | ✅ Resolved |
| m6 | MINOR | UUC2's truly-vanilla user can't reach the first `tctl` invocation | DOC-8 §5, DOC-10 §3.3 | — | ✅ Resolved |
| m7 | MINOR | Controller is `kind=cli` in the manifest yet is a launchd-supervised daemon | DOC-2 (manifest example), DOC-7 §8, DOC-8 §1.1, DOC-5 §7, DOC-9 §5.2 | — | ✅ Resolved |
| m8 | MINOR | UI link-out depends on the `port --json` + `/ui` convention (convention-deep, not contract-deep) | DOC-7 §1/§4.1 | — | ✅ Resolved |
| m9 | MINOR | DNS-rebind guard is sound for browsers but a local non-browser process can still bounce the stack | DOC-7 §6, Resolved-Decision-3 | — | ✅ Resolved |
| m10 | MINOR | `stack health`/`stack doctor` consistency guarantee is a cross-probe race | DOC-4 §4 | — | ✅ Resolved |
| m11 | MINOR | Effort/scope realism: the "thin coordinator" understates the retrofit | DOC-6 §2–§3/§7 | — | ✅ Resolved |

**Item count:** 5 CRITICAL + 15 MAJOR + 11 MINOR = **31**.

---

## CRITICAL

### C1 — `id_from_path` does not canonicalize; ADR-0008's "collision-free, symlink-safe" guarantee is false

- **Severity:** CRITICAL
- **Affected:** ADR-0008 (Decision §1 + Consequences), DOC-3 §8
- **Code-verified:** yes (`crates/trusty-search/src/service/fs_discovery.rs`)
- **The flaw:** the canonical-id helper slugifies the raw path string with no `canonicalize()`, so symlinks / case-insensitive macOS APFS / moved checkouts produce divergent ids for the same directory — the exact collision class the ADR claims to eliminate.
- **Failure mode:** `/Proj` vs `/proj` and symlinked/moved checkouts orphan or duplicate the index/palace.
- **Fix direction:** make canonicalize-then-slug part of the contract; hoist into `trusty_common`; define behavior when `canonicalize()` fails.
- **Status:** ✅ Resolved
- **Decision:** Option A — canonicalize-then-slug folded into a single `trusty_common::canonical_project_id` contract function (canonicalize internally, then slug; the slug never sees a raw path). Defines `canonicalize()`-failure fallback (lexical absolutize + `Fallback` warning, never refuse). Case-insensitive-volume divergence accepted as a named known limitation + tracked follow-up (case-fold only on case-insensitive volumes; deferred). Corrects the overstated "test-proven symlink-safe" claim — that test proves determinism + char-safety only; a symlink-equivalence test is a follow-up.
- **Follow-up:** ADR-0008 updated (Decision §1 → canonicalize-then-slug; new failure-behavior decision point; Consequences/test-claim corrected; case-insensitivity limitation added). DOC-3 §8 updated (canonical rule → canonicalize-then-slug; edge-cases list gains a `canonicalize()`-failure bullet; case-insensitivity limitation noted; overstated test citation fixed). Implementation follow-ups: add symlink-equivalence test; optional case-fold-on-case-insensitive-volume (deferred).

### C2 — DOC-8 launch-hook precedent is misattributed; no claude-mpm startup hook is verified to exist

- **Severity:** CRITICAL
- **Affected:** DOC-8 §4.1/§4.3/Resolved-Decision-4
- **Code-verified:** yes (`crates/trusty-memory/src/commands/setup.rs` installs a **Claude Code** `settings.json` hook — `SessionStart`/`UserPromptSubmit` — not a claude-mpm hook)
- **The flaw:** the UUC1 "auto-config on every launch" mechanism rests on a claude-mpm hook surface the doc evidences only with a different product's feature.
- **Failure mode:** if claude-mpm has no hook surface, the "fallback wrapper" is actually the only viable path — primary/fallback inverted.
- **Fix direction:** re-ground §4 on claude-mpm's real launch surface, or state the hook lands in Claude Code settings and demote the unverified mechanism.
- **Status:** ✅ Resolved
- **Decision:** Option (a) — minimal clarification, not a redesign. The reviewer's premise ("claude-mpm may have no hook surface → primary/fallback invert") is architecturally invalid: claude-mpm is an orchestrator layered on the Claude Code CLI, runs Claude Code underneath, and merges into `~/.claude/settings.json`, so the verified Claude Code `SessionStart` hook fires for claude-mpm sessions and claude-mpm needs no hook surface of its own. The mechanism is unchanged (primary = Claude Code `SessionStart` settings hook; fallback = launch wrapper/alias for non-Claude-Code entry points). Fix is a wording/attribution correction only.
- **Follow-up:** DOC-8 §4.1/§4.3 + Resolved-Decision-4 reworded to correctly attribute the hook to Claude Code (`SessionStart`, verified in `setup.rs`), add the one-sentence "claude-mpm is layered on Claude Code, inherits its hook surface" clarification, and scope the wrapper fallback to non-Claude-Code entry points. No mechanism change.

### C3 — `verbs[]` presence independent of `contract_version` opens an unversioned breaking-change channel

- **Severity:** CRITICAL
- **Affected:** DOC-1 D3 ("versioning split"), ADR-0007 §3, DOC-1 ledger
- **Code-verified:** no
- **The flaw:** a tool can keep `contract_version:1` while incompatibly changing an existing verb's `data` schema; the integer is the only shape signal the controller checks.
- **Failure mode:** graceful-degrade controller deserializes a changed `data` against the v1 struct → panics/drops/misrenders; "never hard-fail" becomes "silently wrong"; ADR-0007's mitigation is "reviewers must catch it" (human discipline).
- **Fix direction:** mandatory integer bump on any incompatible `data` change enforced by a golden-snapshot CI test in `trusty_common`, OR per-verb `data_version` in `verbs[]`.
- **Status:** ✅ Resolved
- **Decision:** Option A — enforce the additive-only / bump-on-break rule with a **golden-snapshot CI test in the `trusty_common` contract module**: serialize a canonical instance of every per-verb `data` struct + the envelope to JSON, commit the snapshots, and gate CI so any change to a serialized shape FAILS unless the snapshot is regenerated AND `contract_version` is bumped AND a ledger row is added (the test asserts they move together). The single integer + integer-comparison negotiation is preserved; a per-verb `data_version` wire axis is **rejected** (it fights ADR-0007's one-axis rationale and is itself a breaking `verbs[]` shape change). The real failure mode is clarified as **version skew across independently-installed crates**: the contract `data` types live in shared `trusty_common` (DOC-1 D6), so within one workspace build producer and consumer compile against the same structs and cannot diverge — the channel only opens when crates are `cargo install`ed per-crate at different `trusty_common` versions, plus the non-Rust claude-mpm Python adapter, which hand-rolls the JSON and can diverge freely. Cross-language coverage split: **Rust tools** gated at CI via the shared-types snapshot test; **claude-mpm / non-Rust members** gated by a captured-output conformance fixture in DOC-10's harness. ADR-0007's "reviewers must catch it" is corrected to machine-enforcement (reviewers are a backstop, not the gate).
- **Follow-up:** ADR-0007 updated (Decision pt 3 gains the golden-snapshot enforcement; Consequences "reviewers must catch it" bullet reworded to machine-enforcement + the version-skew failure mode + the Rust/non-Rust coverage split; new Follow-up bullet for the `trusty_common` snapshot test wired into CI + DOC-10's non-Rust extension). DOC-1 updated (D3 "versioning split" cites the golden-snapshot enforcement + cross-language split; ledger "Rule" notes CI-enforcement via the snapshot test; `trusty_common` contract-module sketch notes it ships the golden-snapshot conformance test). DOC-10 **§2.2 "Conformance pre-gate"** gains a captured-`--json`-vs-golden-schema conformance assertion covering the non-Rust claude-mpm shim. Implementation follow-up: build the golden-snapshot test in `trusty_common` and wire it into CI; capture per-member `--json` fixtures in the harness.

### C4 — DOC-4 dependency de-duplication needs root-cause inference the controller can't do generically

- **Severity:** CRITICAL
- **Affected:** DOC-4 §5.4/§2.3/§8.2 (`clusters[]`)
- **Code-verified:** no
- **The flaw:** the "one root down + N degraded" collapse works for a single edge but breaks on the diamond (review depends on search AND analyze; analyze depends on search) — the stated collapse rule doesn't fire for review→analyze (analyze only degraded), yet the example attributes review to search, requiring transitive root-cause walking the rule never describes; correct attribution is itself domain reasoning.
- **Failure mode:** double-counting or mis-attribution on real transitive/multi-root graphs; `clusters[]` presupposes a clean single-root grouping.
- **Fix direction:** specify the cluster algorithm as a transitive fold over the union graph (`depends_on` ∪ runtime `deps[]`); admit it's a heuristic that can mis-attribute.
- **Status:** ✅ Resolved
- **Decision:** Option A — specify cluster construction as a **transitive fold over the union graph** G = manifest `depends_on` ∪ runtime `deps[]`: identify intrinsic-`down` roots (members broken on their own merits, not merely because a dep is unreachable), then follow each dep-degraded member's proximate `because` pointer **transitively** along required edges to the terminal `down` root(s). Multi-root is supported — a dependent may appear under **>1 cluster** when the walk reaches two distinct `down` roots — while the `summary` still counts each member **exactly once** (anti-double-count holds at the count level, not the cluster-membership level). Implemented by **following the tools' own proximate `because` pointers** (each tool already reports its `deps[]`), not controller-side domain inference, preserving the zero-tool-specific-logic property. Explicitly **admitted as a best-effort heuristic**: a member's `degraded` may be independent of the transitive root it is attributed to, so the attribution is a surfaced *hint*, not a proof; `-v` exposes the raw per-member `deps[]` for verification. Noted that v1's actual graph has only **direct** dependent→root edges (review depends directly on search), so the current direct-edge rule already suffices today — the transitive spec is **correctness-of-claim + future-proofing + multi-root coverage**, not a v1 bug fix.
- **Follow-up:** DOC-4 updated — §5.4's narrow direct-edge collapse rule replaced by the transitive-fold algorithm (build `G`; identify intrinsic-`down` roots; walk proximate `because` pointers transitively to terminal root(s); multi-root membership) + the heuristic admission + the "v1 direct rule already suffices" note; the worked example annotated to make explicit that it collapses cleanly because review depends *directly* on search and that a transitive-only dependent is reached by walking the union graph. §2.3 gains a note (beneath the three-folds table) naming the cluster grouping as a **fourth, orthogonal fold** that does not change verdict counts (each member counted once) but annotates root-cause grouping. §8.2 `clusters[]` prose notes a dependent **may appear under >1 `root`** with **membership-independent** counts (each member counted once regardless of cluster appearances). Implementation follow-up: implement the transitive cluster fold + count-once summary in the controller's rollup.

### C5 — DOC-9 "new versions take effect via connection-safe restart" assumes a `restart` verb DOC-6 says is net-new on every tool

- **Severity:** CRITICAL
- **Affected:** DOC-9 §5.1 vs DOC-6 §2.1–2.4
- **Code-verified:** no
- **The flaw:** the take-effect mechanism dispatches the `restart` contract verb, but DOC-6 marks `restart` ❌ absent/net-new on search/memory/analyze/review; presented as low-risk reuse. Also conflates "drains HTTP requests" (true) with "doesn't interrupt sessions" (false).
- **Failure mode:** `tctl upgrade` silently gates on an unbuilt cross-crate retrofit; reader under-budgets it.
- **Fix direction:** state `restart` is net-new per DOC-6 and the take-effect restart depends on that retrofit; separate request-drain from session-continuity.
- **Status:** ✅ Resolved
- **Decision:** Option A — honest scoping, no design change. Keep restart-as-verb (per-OS knowledge in the member; controller stays tool-agnostic). DOC-9 §5.1 reframed to distinguish the grounded primitives (launchd bootout/bootstrap, #534 graceful drain in `trusty_common`) from the net-new `restart` contract verb, which DOC-6 §2.1–2.4 marks ❌ absent on every daemon (new `commands/restart.rs` on search/memory/analyze; review heaviest) — so take-effect depends on that DOC-6 T2-lifecycle retrofit, not free reuse; cross-linked to the m11 / DOC-6 §2 retrofit-scope realism. Request-drain (in-flight HTTP requests complete, #534) separated from session-continuity (NOT preserved — MCP/Claude Code sessions interrupted, `mcp_bridge` reconnects with backoff, in-flight session state lost), reconciling §5.1/§5.3 with the §3.2 blast-radius warning. B/C (controller-driven supervisor restart to avoid the net-new verbs) rejected for v1: would move per-OS supervision knowledge into the controller and ripple into DOC-1/5/6.
- **Follow-up:** DOC-9 §5.1 + §5.3 reworded (net-new `restart` verb + DOC-6 dependency + m11 cross-link; request-drain vs session-continuity separation). No mechanism change. Implementation follow-up already tracked in DOC-6 §2.1–2.4 (the per-tool `restart` retrofit) and DOC-11 m11 (effort realism).

---

## MAJOR

### M1 — "Zero tool-specific logic" is oversold (relocated, not eliminated)

- **Severity:** MAJOR
- **Affected:** spec §83; DOC-2 §6, DOC-5 §1/§2.2, DOC-6 §4, DOC-3 §7, DOC-7
- **Code-verified:** no
- **The flaw:** hard-coded install-source templates, the claude-mpm shim, `SKIP_UI_BUILD` derivation, `/ui`+`port --json` convention, and `config` precedence are tool-class knowledge in the shipped artifact.
- **Failure mode:** misleads maintainers into thinking any conformant tool "just works."
- **Fix direction:** reword to "zero tool-specific *verb-dispatch* logic" + enumerate the bounded tool-class assumptions.
- **Status:** ✅ Resolved
- **Decision:** best recommendation — reframe the property precisely as **"zero per-tool verb-dispatch logic"** (no per-named-tool branching; the controller hard-codes no tool identity and discovers capabilities at runtime via `verbs[]`), and add ONE canonical enumeration (DOC-5 §2.2.1) of the bounded tool-CLASS assumptions the shipped artifact does carry — install-source templates (`install.source` = `cargo` / `python` / `uv`), the orchestrator shim (keyed off `kind`, not the name claude-mpm), the `SKIP_UI_BUILD` / `ui.available` derivation, the `/ui` + `port --json` UI convention, and config precedence — each keyed off a **manifest field or `kind`**, never a tool identity. A definitional-anchor sentence (DOC-5 §2.2.2) governs the ~20 other shorthand occurrences across the set (no 25-line churn); the two load-bearing headline claims (DOC-5 intro, README) are reworded with a cross-ref. Honest framing: the *dispatch engine* is genuinely generic (no per-named-tool branching); tool-class knowledge was **relocated** into manifest-keyed templates/conventions, **not eliminated** — so adding or swapping a *named* tool needs zero controller code, but a new tool *class* (e.g. a new `install.source`) is the bounded exception. Per-verb `data_version`-style alternatives and a 25-occurrence rewrite were not pursued; the anchor governs the rest.
- **Follow-up:** DOC-5 §2.2 (heading + opening claim sharpened to "zero per-tool verb-dispatch logic"; proof table kept) + new §2.2.1 (bounded tool-class-assumptions enumeration, each keyed off a manifest field / `kind`) + new §2.2.2 (definitional-anchor sentence governing the stack-wide shorthand). DOC-5 intro + README headline claims reworded with a cross-ref to DOC-5 §2.2. The ~20 other shorthand occurrences across the set are governed by the §2.2.2 anchor, **not** individually edited. No code change.

### M2 — `stack_version` tuple will rot (no test owner, no CI gate, embedded in the binary)

- **Severity:** MAJOR
- **Affected:** DOC-2 §4, Resolved-Q5
- **Code-verified:** no
- **The flaw:** no mechanism/owner/CI to materialize-and-test a candidate tuple; per-#343 independent versioning staleness; the BOM is compiled into `tctl` so new pins require a controller release.
- **Failure mode:** "keep current with minimal effort" silently degrades to "current as of last tctl release."
- **Fix direction:** a stack-integration CI matrix that tests candidate tuples; consider decoupling the BOM from the binary (the deferred remote channel).
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — anchor "tested tuple" to a green DOC-10 acceptance run against that exact pin set (the harness already IS the end-to-end tuple test, so no separate stack-integration matrix is net-new). Name the owner/cadence as a scheduled + `workflow_dispatch` CI job that materializes a candidate tuple (latest-published per crate, or a curated set), runs the harness, and on green promotes a new `stack_version`. Acknowledge the BOM-in-binary staleness explicitly ("current" = as of the last `tctl` release); the system-override `manifest.toml` is the interim pin; the **remote BOM channel stays deferred for v1** (DOC-2 §8) but is named as the eventual decoupling so a freshly-tested tuple can ship without a `tctl` rebuild; "no tested tuple yet for the newest crates" is surfaced as **drift, not failure**. Remote-channel-now (Option B) **rejected** for v1 — a hosted fetch plus its trust/signature surface is exactly the DOC-2 §8 deferral.
- **Follow-up:** DOC-2 §4 (new "How a tested tuple is materialized (owner + gate)" subsection — harness as the tested-tuple gate, scheduled + `workflow_dispatch` owner/cadence, BOM-staleness + interim system-override + deferred remote-channel framing). DOC-10 §6b (new table row: harness as the stack-tuple promotion gate on a scheduled + `workflow_dispatch` cadence). Adjacency noted with [M14](#m14--ci-tests-the-wrong-paths-for-the-primary-target) (CI path coverage). Implementation follow-up: the candidate-tuple CI job + promotion flow.

### M3 — Cross-repo claude-mpm dependency is load-bearing in 4 places but never assessed as one systemic risk

- **Severity:** MAJOR
- **Affected:** DOC-6 §4, DOC-8 §4/§5, DOC-9 §3.4, DOC-10 §6
- **Code-verified:** no
- **The flaw:** uv install + launch hook + output-parsing shim + version pin all couple to an external evolving repo; the shim parses *human* `mpm-doctor` text while install floats to *latest* (unpinned) — a contradiction; best-effort parsing degrades silently.
- **Failure mode:** an upstream cosmetic change misclassifies orchestrator health and the every-PR CI (frozen pin) won't catch it.
- **Fix direction:** a single cross-cutting "claude-mpm external-dependency risk" section + a shim contract-test that fails loudly on drift; reconcile unpinned-install vs version-coupled-parser.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — add a single **consolidated claude-mpm external-dependency risk** section (DOC-6 §4.1) enumerating the **4 coupling points** (`uv` install/upgrade, launch hook, output-parsing shim, version pin) + blast radius + mitigation, framing them as **one systemic risk** with the **output-parsing shim as the most fragile point**. Reconcile the unpinned-install-vs-version-coupled-parser **contradiction**: the orchestrator installs the **BOM-pinned** version (NOT latest, exactly like every other cargo member installs its BOM `version`) since the shim's parsing is version-coupled, so install and shim move in **lockstep**; `--latest` becomes an **opt-in** move that marks the stack **drifted** (M2/M11 framing). This fixes DOC-8 §1.4 + Resolved-Decision-2's prior "install latest" framing. Add a shim **captured-output contract-test** (DOC-10 §6.1/§2.2, alongside the C3/M2 fixtures) that **fails loudly in CI** on a claude-mpm CLI-format drift, plus a **loud-degrade runtime rule**: unrecognized claude-mpm output → `degraded` with a clear message ("claude-mpm output format unrecognized — shim may be stale vs the installed version"), **never** a confident-but-wrong health verdict. So drift surfaces loudly (CI failure or visible `degraded`) instead of silently misclassifying orchestrator health.
- **Follow-up:** DOC-6 §4.1 (new consolidated external-dependency-risk section — 4 coupling points + blast radius + mitigation + loud-degrade runtime rule), §5 (BOM-pinned install reconciled — not latest; lockstep with shim; `--latest` = opt-in drift). DOC-8 §1.4 + Resolved-Decision-2 (install the BOM-pinned version, not latest). DOC-10 §6.1 (new shim captured-output contract-test) + §2.2 (existing captured-`--json` conformance pre-gate cross-referenced). Implementation follow-up: build the shim drift contract-test + the loud-degrade behavior; BOM-pin the orchestrator install (`uv tool install claude-mpm==<pin>`).

### M4 — `config` verb is a breaking CLI change + a hole in the read-only guarantee

- **Severity:** MAJOR
- **Affected:** DOC-1 `config.data`, DOC-5 §1.2, DOC-6 §2.6, DOC-3 Q3
- **Code-verified:** yes (`crates/trusty-search/src/main.rs` — existing `config get/set` mutates live daemon limits)
- **The flaw:** overloading the contract's read-only `config` onto a tool that ships a mutating `config set` is semver-relevant; passthrough (`tctl <tool> config set`) forwards mutations through a "read-only config" controller.
- **Failure mode:** a UI/agent mutates via passthrough despite the read-only posture.
- **Fix direction:** rename the mutating verb (e.g. `tune`/`limits`); define passthrough's stance on mutating subcommands.
- **Status:** ✅ Resolved
- **Decision:** Option A — make the read-only `config` guarantee true at the contract boundary. The contract `config` verb = read-only `ConfigData`, accepting only read selectors (`--scope`, optional single-key projection) and no mutating arguments. trusty-search's live memory-limit mutation moves to a separate, non-contract, non-advertised `tune` verb (canonical); `config set`/`config tune` are kept as deprecated tool-native back-compat aliases (trusty-search is 0.x — emit a deprecation note, no hard break). Passthrough is **verb-aware** — it forwards an advertised contract verb with its contract-defined arguments, not a blind arg shell — so mutation under the non-advertised `tune` verb is **NOT reachable through the controller** in v1 (read-only by construction). The DOC-5 §3 blast-radius gate is broadened from the enumerated four (`install`/`upgrade`/`restart`/`stop`) to the mutating-verb class as defense-in-depth, so any future advertised mutating verb is confirmation-gated. DOC-3 §7's contradictory "MAY dispatch a tool's `config`-write subcommand" permission is reconciled to "not exposed via the controller in v1." DOC-6 §2.6 already handled the naming half; this finishes it and closes the passthrough hole.
- **Follow-up:** DOC-6 §2.1 config row + §2.6 (canonical non-contract `tune` verb + deprecated `config set`/`config tune` aliases, not advertised in `verbs[]`). DOC-1 `config.data` (read-only, read selectors only, mutation is non-contract). DOC-3 §7 (reconciled — no controller-exposed mutation in v1). DOC-5 §1.2 (read-selectors note), §2 (verb-aware passthrough; non-advertised verbs not forwarded), §3 (blast-radius gate over the mutating class). Implementation follow-up: trusty-search adds the `tune` verb + deprecation aliases on `config set`/`config tune`; controller passthrough forwards only contract-defined verb args.

### M5 — clap `external_subcommand` passthrough collides with first-class subcommands + controller-as-member recursion

- **Severity:** MAJOR
- **Affected:** DOC-5 §6/§1.1
- **Code-verified:** no
- **The flaw:** `tctl trusty-controller restart` routes passthrough → `tctl restart` (self); global flags after the verb get swallowed into forwarded args; "sugar over passthrough" under-specified.
- **Failure mode:** recursive/ambiguous dispatch requiring name-special-casing (the thing the design forbids).
- **Fix direction:** document controller-as-member exclusion + flag-position rule, or use explicit `tool/rest` positionals.
- **Status:** ✅ Resolved
- **Decision:** best recommendation — keep the clap `external_subcommand` passthrough idiom and document three guardrails. (1) **Controller-as-member exclusion:** `tctl <controller-id> <verb>` (id = `trusty-controller`) is rejected with exit `3`; the controller's own lifecycle is the first-class `restart`/self-upgrade surface (DOC-9 §8), not a passthrough to itself — this prevents recursive self-dispatch, and the dispatcher recognizing its own id is self-identity, not per-tool branching. (2) **Flag-position rule:** controller global flags MUST precede the member token, since `external_subcommand` forwards everything after the member id verbatim to the member. (3) **Reserved-name manifest-validation:** member ids MUST NOT collide with the first-class command names (which clap resolves before `external_subcommand`), checked when the manifest is loaded. The explicit `tool/rest` positional redesign is **rejected** as unnecessary given the guardrails.
- **Follow-up:** DOC-5 §6 Notes (three guardrail bullets — controller-as-member exclusion, flag-position, reserved-name validation) + §2 passthrough error rules (controller-id-not-a-target + flag-position). Implementation follow-up: the dispatcher rejects the controller's own id; the manifest loader validates member ids against the reserved first-class command names.

### M6 — CLI-as-authority (D1) under-specified for "daemon up but wedged"

- **Severity:** MAJOR
- **Affected:** DOC-1 D1, `health.data`
- **Code-verified:** no
- **The flaw:** no timeout/`unknown` state — a hung daemon that connects but never responds is neither cleanly `down` nor `running`.
- **Failure mode:** `tctl stack doctor` hangs or misreports a wedged daemon as running (PID lockfile exists).
- **Fix direction:** mandatory per-verb invocation timeout + a `degraded`/`unknown` mapping for slow/no-response.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — DOC-1 D1 clarified: **liveness = answering the authoritative probe within the timeout, not process existence**; a stale-PID/bound-port daemon that does not answer is `down`. A wedged/timeout daemon maps to `down` (four-value vocabulary preserved) with a `reason` discriminator (`timeout`/`wedged`/`unreachable` vs `not_running`) so remediation differs (restart vs start). No fifth `unknown` verdict (rejected: a wedged daemon is operationally unusable = `down`; the `reason` field carries the nuance).
- **Follow-up:** DOC-1 D1 (liveness = answering) + `health.data` (`reason` field on the synthesized envelope). DOC-4 §1.3 (synthesized terminal envelope carries `reason`). Implementation follow-up: controller stamps `reason` on synthesized-`down` envelopes and routes remediation (restart vs start) off it.

### M7 — Concurrent "ensure every launch" race on a shared daemon

- **Severity:** MAJOR
- **Affected:** DOC-3 §4/§6/§8
- **Code-verified:** no
- **The flaw:** two simultaneous project launches both see "daemon down" and both `start`; no system-scope lock (TOCTOU).
- **Failure mode:** double-start, port-bind/lock contention, install/upgrade triggered mid-ensure.
- **Fix direction:** system-scope advisory lock around the system-rung ensure; define loser behavior.
- **Status:** ✅ Resolved
- **Decision:** best recommendation — a **system-scope advisory lock** (a lockfile in the system state dir, e.g. `~/.config/trusty-controller/ensure.lock`, via the `fs4`/flock primitive the daemons already use for their PID lockfiles) around **only** the system-rung ensure actions (shared-daemon `start`, `install`, `upgrade`). The loser waits (bounded) for the holder, then re-runs CHECK and no-ops (the holder has already started the daemon), degrading to a clear error on timeout — never an independent second `start`. The project-rung ensure stays lock-free: those ops are idempotent and key on distinct per-project ids, so concurrent project ensures do not contend. The daemon's PID lockfile is a secondary backstop; the advisory lock makes the loser a clean no-op rather than relying on port-bind failure; composes with M12 verify-after.
- **Follow-up:** DOC-3 §4 (system-scope advisory-lock subsection: lock scope = system rung only, loser waits-then-rechecks-then-no-ops, project rung lock-free by distinct keys, PID-lock backstop + M12 cross-ref). Implementation follow-up: `fs4` advisory lock around system-rung ensure with bounded wait + re-check.

### M8 — Status-enum reconciliation is ambiguous

- **Severity:** MAJOR
- **Affected:** DOC-1 D4, DOC-4 §1.1/§4, DOC-3 §9
- **Code-verified:** no
- **The flaw:** `health{running|degraded|down}` vs `doctor{ok|warn|fail|pending|skipped}` have no defined total order; `stack health` and `stack doctor` can disagree in direction.
- **Failure mode:** contradictory verdicts / CI exit-code ambiguity (both warn and degraded → exit 2).
- **Fix direction:** define one total order across the union; frame health/doctor as fast/deep views of one lattice.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — affirm DOC-4 §2.0's four-value verdict as the **single total-order lattice** both source vocabularies (`health`/`doctor`) map into (fast vs deep views of one order); add the both-envelopes reconciliation rule — when the rollup holds both a `health` and a `doctor` envelope for one member, the cell is the **worst-wins fold** of the two mapped verdicts, so they can never disagree in direction.
- **Follow-up:** DOC-4 §2.0 (single-lattice framing + both-envelopes worst-wins fold), §4 (health/doctor as fast/deep of one lattice; the legitimate difference is scope-depth, not direction). DOC-1 D4 (note the four-value lattice + `reason` discriminator).

### M9 — D8 secret redaction is unenforceable

- **Severity:** MAJOR
- **Affected:** DOC-1 D8, DOC-6 conformance clause 5
- **Code-verified:** no
- **The flaw:** tools self-report already-redacted JSON; controller can't verify; helper is opt-in; no CI gate.
- **Failure mode:** one buggy tool (e.g. the shim) leaks a key/credential into the web UI.
- **Fix direction:** defense-in-depth controller-side redaction pass over envelope strings before render.
- **Status:** ✅ Resolved
- **Decision:** best recommendation — defense-in-depth, honestly scoped. Tool-side `redact_value` (DOC-1 D8 / DOC-6 §3) stays the **primary line**: tools redact at the source via the shared helper before emitting any envelope value. The controller adds a **belt-and-suspenders redaction pass** over **all** envelope string values **before render** — on both the CLI output and the DOC-7 UI — masking high-confidence secret patterns (`AKIA…` AWS access keys, `Bearer` / `Authorization` values, `scheme://user:pass@host` credentials, key prefixes `sk-` / `ghp_` / `xox…`, long high-entropy blobs) with `***redacted***`, so a tool or the claude-mpm shim that forgot to redact **cannot leak** a secret into the UI. A **negative CI conformance assertion** (DOC-6 §8 / DOC-10 captured-output fixtures, alongside the C3/M2/M3 fixtures) asserts that **no known secret pattern appears unredacted** in any member's captured envelope output — including the shim — and **fails CI loudly** if one does, so a redaction bug is caught pre-merge, not in the live UI. **Honestly scoped:** pattern matching is necessarily **heuristic** — it cannot catch every secret shape — so the controller-side pass is **defense-in-depth, not a guarantee**; tool-side `redact_value` remains the primary line and the negative CI assertion is the gate (a nod to M1's honesty principle).
- **Follow-up:** DOC-1 D8 (layered/defense-in-depth redaction — tool-side `redact_value` primary + controller-side belt-and-suspenders pass over envelope strings on CLI + UI + the heuristic/not-a-guarantee caveat). DOC-6 §1 clause 5 (redaction both happens **and** is verified by the negative CI assertion; controller pass = runtime backstop) + §8 (negative secret-pattern assertion in the self-check redaction lint and the captured-output fixtures, claude-mpm shim included, consistent with the C3/M2/M3 fixture framing). Implementation follow-up: the controller-side redaction pass (CLI + UI) + the negative-assertion conformance test in the harness.

### M10 — Timeouts (2s health / 10s doctor) false-fail cold / model-loading daemons

- **Severity:** MAJOR
- **Affected:** DOC-4 §1.3, Resolved-Q1
- **Code-verified:** no
- **The flaw:** ONNX model load, cold start, or graceful-shutdown window blow the fixed timeouts → synthesized `down`.
- **Failure mode:** `tctl stack doctor` CI gate false-fails a healthy-but-slow cold stack; a gracefully-restarting daemon reads as down.
- **Fix direction:** cold-start budget / warmup ping; ship per-member timeout overrides day one.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — two parts: (1) an up-but-not-ready daemon (cold/model-loading/warming/mid-graceful-restart) MUST answer `health` **promptly** with `degraded`/`pending` + a `detail` rather than hanging, so warming is reported, not timed out into a false `down` (the restart window maps to `pending`, consistent with C5); the state lives in the tool, preserving zero-tool-specific-logic. (2) **Un-defer per-member timeout overrides** in the manifest day one (DOC-2 §3), precedence per-member > global `--timeout` > 2 s / 10 s defaults.
- **Follow-up:** DOC-4 §1.3 (warming/restart not-`down`; per-member timeouts day-one; precedence). DOC-1 `health.data` (prompt warming/restarting reply requirement). DOC-2 §3 (per-member `timeout` field + trusty-search worked-example hint). Implementation follow-up: tools report warming/restarting health states promptly; controller honors per-member timeouts.

### M11 — `state.toml` stack-version tracking can silently desync; UI shows stale "clean"

- **Severity:** MAJOR
- **Affected:** DOC-9 §4.4, Resolved-Q3, DOC-7 §2.1
- **Code-verified:** no
- **The flaw:** crash mid-upgrade, no locking (UI polls every 10s while CLI upgrades), lazy reconcile only on `tctl status`.
- **Failure mode:** UI shows "on 2026.07-1 ✓" while two members are still old.
- **Fix direction:** derive displayed version live from `version --json` (state.toml = cache only); write atomically + advisory-locked.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — make the cache-only + live-reconcile design **normative and universal**: the clean/drift verdict is ALWAYS derived live (compare each member's `version --json` against the labeled tuple's pins), never from the persisted label, on **all surfaces** (the CLI *and* the DOC-7 UI). `state.toml` is a label cache only; writes are **atomic (temp-file + rename) + advisory-locked** so a UI poll mid-upgrade never reads a torn label and concurrent writers cannot corrupt it; an **in-progress/partial marker** is written during an upgrade so a crash mid-upgrade cannot leave a falsely-"clean" cached label. The `tctl version --json` / `tctl stack health --json` payloads carry the live-reconciled drift verdict the UI renders.
- **Follow-up:** DOC-9 §4.4 + Resolved-Decision-3 (normative live-derivation across all surfaces, atomic + advisory-locked `state.toml` writes, in-progress/partial marker). DOC-7 §2.1 (the dashboard renders the live-reconciled `stack_version` verdict from `version --json` / `stack health --json`, never the raw persisted label). Adjacency noted with [M12](#m12--no-auto-rollback--non-transactional-installs-can-wedge-a-partial-tuple) (partial-tuple as a first-class outcome). Implementation follow-up: atomic + advisory-locked `state.toml` writes + in-progress marker + live-reconcile in the version/health payloads.

### M12 — No auto-rollback + non-transactional installs can wedge a partial tuple

- **Severity:** MAJOR
- **Affected:** DOC-9 §6/§3.6, Resolved-Q5
- **Code-verified:** no
- **The flaw:** per-member health-gate defends the single-member case, not the cross-member case (new-search + old-analyze = untested tuple; no tuple-level gate).
- **Failure mode:** drifted, untested, partially-new stack; recovery is manual `cargo install @old` (defeats zero-knowledge).
- **Fix direction:** gate dependent restarts on the dependency's verify-after; make "drifted/partial-tuple" a first-class outcome with one-command pin-back.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — gate dependent restarts on the dependency's verify-after: a dependent is only upgraded/restarted after its dependency reaches target *and* verifies healthy, and a **failed** dependency holds its dependents at the last known-good combination, reported `blocked-by` the root (this extends §6's dependency exception from "dep down" to "dep upgrade incomplete"), so an untested new + old pair cannot form silently. Make drifted/partial-tuple a **first-class outcome** (reusing the M11 partial `state.toml` marker) surfaced as a distinct DOC-4 *partial tuple — not a tested combination* verdict, with **one-command pin-back** to the last known-good tested tuple (`tctl upgrade --to <last-good-stack_version>`, the whole-tuple form extending the existing per-member `--version` downgrade + §4.2 stack-move surface). This is **NOT** auto-rollback (Resolved Decision 5 unchanged) — a zero-knowledge-friendly **manual** pin-back replacing the per-crate `cargo install <crate>@<old>` incantation; the known-good tuple is the M2 tested BOM.
- **Follow-up:** DOC-9 §3.6 (verify-after gating in ordering), §6 (cross-member gap + drifted/partial-tuple first-class + one-command pin-back), Resolved-Decision-5 (no-auto-rollback retained but partial-tuple recovery is now one-command pin-back). Cross-ref [M2](#m2--stack_version-tuple-will-rot-no-test-owner-no-ci-gate-embedded-in-the-binary) (tested tuple) + [M11](#m11--statetoml-stack-version-tracking-can-silently-desync-ui-shows-stale-clean) (partial marker). Implementation follow-up: verify-after gate between dependency and dependents; `tctl upgrade --to <stack_version>` whole-tuple pin-back; surface the partial-tuple verdict.

### M13 — Self-upgrade abandons in-flight UI op state

- **Severity:** MAJOR
- **Affected:** DOC-9 §8, DOC-7 §8/§3.2
- **Code-verified:** no
- **The flaw:** the SSE replay buffer lives in the dying process's memory; respawn has no replay for the pre-restart op.
- **Failure mode:** browser sees a gap with no `complete` event; UI stuck "reconnecting…" for an op that finished.
- **Fix direction:** persist op terminal-state, or make self-upgrade CLI-only in v1.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — UI-initiated upgrades **exclude the controller self-step in v1** (reuse the existing `--exclude-self`, DOC-9 §8): the UI upgrades the *other* members (their op state survives because the tracking controller does not restart mid-stream) and hands the controller's own self-upgrade off to the CLI with a "run `tctl upgrade` in a terminal" message, rather than a self-exit that destroys the in-memory SSE replay buffer and strands the op (browser never sees `complete`, hangs reconnecting). **Root cause:** the process tracking the op must not be the process that dies. Persisting op terminal-state across respawn (Option B) is **deferred** as the future path to UI-initiated self-upgrade.
- **Follow-up:** DOC-7 §3.2 (self-restart op cannot be tracked across respawn → UI excludes self), §8 (UI uses `--exclude-self`, CLI handoff for controller self-upgrade, persist-op-state deferred). DOC-9 §8 reuse of the existing `--exclude-self` escape hatch. Implementation follow-up: UI upgrade flow passes `--exclude-self` + CLI-handoff messaging; (deferred) persist op terminal state for UI self-upgrade.

### M14 — CI tests the wrong paths for the primary target

- **Severity:** MAJOR
- **Affected:** DOC-10 §4.1/§6b, DOC-8 §6, Resolved-Decision-5/6
- **Code-verified:** no
- **The flaw:** every-PR Linux leg uses foreground `serve` (production = systemd-user, tested by no one per-PR); macOS is "primary" but its gate (incl. the cdhash SIGKILL trap) runs only nightly.
- **Failure mode:** supervision/restart regressions and the macOS `cp`-into-PATH SIGKILL regress green and ship to nightly discovery.
- **Fix direction:** minimal per-PR macOS smoke (install + health + cdhash assertion) + a systemd-user leg per-PR.
- **Status:** ✅ Resolved
- **Decision:** Option A — rebalance the CI gate so the primary target's critical paths get per-PR signal at bounded cost (a minimal smoke, NOT the full §2 acceptance matrix). Add a **minimal per-PR macOS smoke** (`cargo install trusty-controller --locked` → daemon up under launchd → `health --json` == `running` → **cdhash assertion** that the on-PATH binary execs without SIGKILL via `cargo install`'s atomic-rename path and the forbidden `cp`-over path is NOT used → one `restart` via `bootout`→`bootstrap` → `health` again; NOT the full §2 scenario, which stays nightly) so the primary target + the silent cdhash/`cp`-SIGKILL trap are caught **pre-merge** instead of nightly post-merge. Add a **per-PR systemd-user leg on a standard ubuntu runner** (which has `systemctl --user`) so the real Linux v1 product supervision path (Resolved-Decision-6) is gated per-PR, with the foreground-container leg retained for the fallback branch. The full §2 macOS acceptance run + the M2 stack-tuple promotion gate stay **nightly/scheduled** (unchanged). Trade-off acknowledged: a few bounded macOS minutes per PR vs post-merge discovery of a silent-SIGKILL regression.
- **Follow-up:** DOC-10 §6b (new per-PR macOS-smoke row + per-PR systemd-user-runner row; existing macOS row clarified as the nightly full §2 run; `isolation.yml` description updated to include the per-PR macOS-smoke + systemd-user-runner jobs), §4.1 (systemd-user product path now gated per-PR on a standard ubuntu runner, the container leg covering the foreground/fallback branch only, the VM leg becoming a deeper/optional nightly validation). DOC-8 §6 + Resolved-Decision-6 (primary target + systemd-user product path gated per-PR per DOC-10). Adjacency noted with [M2](#m2--stack_version-tuple-will-rot-no-test-owner-no-ci-gate-embedded-in-the-binary) (the M2 stack-tuple promotion gate stays nightly). Implementation follow-up: the `isolation.yml` per-PR macOS-smoke + systemd-user-runner jobs incl. the cdhash assertion.

### M15 — Project-identity migration orphans existing index/palace state

- **Severity:** MAJOR
- **Affected:** DOC-6 §7, DOC-3 §8, ADR-0008
- **Code-verified:** no
- **The flaw:** the basename→slug flip re-keys every existing index/palace; DOC-6 treats it as a refactor, not a stateful data migration; both id forms are currently live in the daemon registry for the same root.
- **Failure mode:** first `tctl ensure` re-indexes everything from scratch (multi-minute "pending", storage doubling).
- **Fix direction:** explicit alias/re-key migration step in the trusty-search retrofit.
- **Status:** ✅ Resolved
- **Decision:** best recommendation — an explicit, idempotent, crash-safe **re-key migration** carried by trusty-search's EXISTING forward-only `_meta` schema-migration framework (`core::migration`), **NOT** a from-scratch reindex. Because colocated index data is `root_path`-addressed (the daemon stores `ColocatedIndexEntry { root_path, id }` and the on-disk `.trusty-search/` lives under the root, so the id is only the in-memory registry key), the migration recomputes the canonical slug from each index's `root_path` and **re-keys the registry entry in place**, reusing the existing redb/usearch data (**no re-embed**), and drops the duplicate old-form registration (basename and/or divergent slug) for the same root; trusty-memory palaces get an **alias/rename** (old-id → canonical-slug). A `schema_version` bump carries it; an already-migrated registry is a no-op; a crash mid-migration retries safely under the framework's existing guarantee. Outcome: the first `tctl ensure` after the flip sees the project as `exists`/`fresh`, **not** a multi-minute `pending` rebuild with storage doubling. This closes the M3/m11 work-breakdown item 7 (previously "design deferred to M15").
- **Follow-up:** DOC-6 §7.1 (new "Stateful re-key migration (no reindex)" subsection — re-key by `root_path`, reuse data in place, drop the duplicate old-form registration, palace alias/rename, carried by `core::migration` + a `schema_version` bump), DOC-6 §3.1 item 7 + Remaining-work checklist item 7 (now designed in §7.1, no longer deferred). ADR-0008 Consequences "Migration required" bullet (the migration is a stateful re-key, **not** a reindex). DOC-3 §8 edge-cases (new bullet: existing state is re-keyed, not orphaned). Implementation follow-up: the trusty-search re-key migration + the trusty-memory palace alias as part of the project-identity retrofit.

---

## MINOR

### m1 — Aggregate exit code undefined for fan-out `stack doctor`

- **Severity:** MINOR
- **Affected:** DOC-1 D5, DOC-4
- **Code-verified:** no
- **The flaw:** single-member exit codes defined, but the controller's own aggregate code across N members (mix of 3/1/2/0) isn't.
- **Failure mode:** CI scripts get inconsistent signals; contract-incompatible (install problem) indistinguishable from fail (runtime).
- **Fix direction:** define aggregate precedence (e.g. `3 > 1 > 2 > 0`) in DOC-1 D5 / DOC-4.
- **Status:** ✅ Resolved
- **Decision:** define the aggregate exit-code precedence `3 ≻ 1 ≻ 2 ≻ 0` (worst-wins across the N members of a fan-out): any contract-incompatible / below-floor member (or a controller/usage problem) → `3`; else any runtime `down` → `1`; else any `degraded` → `2`; else `0`. Project `pending` stays `0`. This promotes a contract/install problem **above** a runtime daemon-down so the two are distinguishable from the exit code alone — a contract-incompatible member contributes `3` (install/upgrade remediation), distinct from a runtime `down`=`1` (start/restart remediation). The verdict **lattice** for rendering is unchanged (a contract-incompatible member's cell still renders `down`, since it is unusable); only the **exit code** promotes contract/usage problems to `3`.
- **Follow-up:** DOC-4 §7 (aggregate exit-code precedence subsection added) + DOC-1 D5 (one-line note that the stack aggregate exit code is the worst member code by `3 ≻ 1 ≻ 2 ≻ 0`, where a contract-incompatible member contributes `3`, distinct from runtime `down`=`1`). Implementation follow-up: the controller computes the fan-out exit code by this precedence.

### m2 — `pending` is gameable (no stall detection)

- **Severity:** MINOR
- **Affected:** DOC-3 §2, DOC-1 `doctor.data`, Resolved-Q5
- **Code-verified:** no
- **The flaw:** freshness is tool-self-reported and `pending` never worsens the rollup; a crash-looping indexer can report `pending` forever.
- **Failure mode:** "usable now, ready in ~Ns" becomes permanently pending while doctor stays green-ish.
- **Fix direction:** require `pending` to carry `pending_since`/`progress_pct`; controller escalates a long-stalled `pending` to `warn` by elapsed time (without inspecting the index).
- **Status:** ✅ Resolved
- **Decision:** a `pending` doctor check carries an optional `pending_since` (an ISO-8601/epoch timestamp of when the pending state began) plus an optional advisory `progress_pct` (`0`–`100`, display only). The controller (DOC-4) escalates a `pending` whose `pending_since` is older than a **bounded elapsed-time staleness budget** to `degraded`, **purely time-based** with no index introspection (freshness stays tool-reported, DOC-3 Q5) — so a crash-looping or stalled indexer is no longer perpetually green-ish but rolls up as `degraded` (stalled). `progress_pct` is advisory and never drives the verdict. If a check omits `pending_since`, the controller cannot time-escalate it and it stays `pending` (degrades to current behavior — no regression).
- **Follow-up:** DOC-1 `doctor.data` (optional `pending_since`/`progress_pct` on the check schema, JSON block, field notes, worked example, and the `DoctorCheck` sketch). DOC-4 §5.5 (new "Stalled `pending` → time-escalated to `degraded`" subsection — elapsed-time-only escalation, no introspection, absent-`pending_since` fallback) + §2.0 verdict table (stalled-`pending` listed as a `degraded` source). Implementation follow-up: tools emit `pending_since`; the controller time-escalates stalled `pending` against the staleness budget.

### m3 — Project-scope `fail`/`down` exits 0, masking a real local outage

- **Severity:** MINOR
- **Affected:** DOC-4 §2.2/§7, Resolved-Q3
- **Code-verified:** no
- **The flaw:** a genuine project `fail` (not `pending`) keeps exit 0 (system-track-driven), same code as a healthy stack.
- **Failure mode:** a corrupt index in this checkout never blocks a scripted launch; only signal is a buried glyph.
- **Fix direction:** opt-in `--fail-on-project` or a louder summary; stop folding `fail` into the `pending`-is-not-broken framing.
- **Status:** ✅ Resolved
- **Decision:** keep the **default exit `0`** for a genuine project `fail`/`down` (system-track-driven — a project problem's blast radius is one repo, so a scripted machine-health gate should not fail on it), plus three changes: (1) add an opt-in **`--fail-on-project`** flag that makes a genuine project `fail`/`down` (NOT `pending`) drive a non-zero exit; (2) surface a **louder summary line** for a project `fail`/`down`, distinct from a buried glyph; (3) stop folding a project `fail` into the "pending is not broken" framing — a corrupt-index `fail` is a real local problem, distinct from a not-yet-indexed `pending`. This resolves Open Question 3.
- **Follow-up:** DOC-4 §2.2 (project-`fail`-vs-`pending` distinction + louder summary + `--fail-on-project` opt-in) + §7 (`--fail-on-project` opt-in folded into the aggregate precedence; Open Question 3 resolved: default exit `0`, opt-in `--fail-on-project`, louder project-`fail` summary). Implementation follow-up: the `--fail-on-project` flag + the louder project-`fail` summary line in the controller renderer.

### m4 — `scope` vs config-provenance `scope` overload is leaky + stringly-typed

- **Severity:** MINOR
- **Affected:** DOC-1 D7 + `config.data` (`ConfigSource.scope: String`)
- **Code-verified:** no
- **The flaw:** two `scope` fields with overlapping-but-different value sets in one JSON doc; provenance is an untyped string (`env` can't reuse the `Scope` enum).
- **Failure mode:** authors conflate them; controller can't catch typos.
- **Fix direction:** rename provenance to `origin` (`{env|project|system}`) with its own enum; one `scope` axis.
- **Status:** ✅ Resolved
- **Decision:** Rename the config-provenance field `config.data` `sources[].scope` → `sources[].origin`, with its own enum `{env|project|system}` (a distinct, typed provenance vocabulary), leaving `scope` used **exclusively** for the D7 wire axis `{project|system|all}`. One `scope` axis + a separate typed `origin`, so authors cannot conflate the two axes and a typo in `origin` is catchable against its own enum rather than silently accepted as a stringly-typed `scope`.
- **Follow-up:** DOC-1 `config.data` (rename `sources[].scope` → `sources[].origin` in the illustrative JSON + the worked example; rewrite the "Two distinct scope vocabularies" note as "`scope` = D7 wire axis; `origin` = config provenance") + D7 (one-line note that provenance is the separate `origin` field) + the `trusty_common::contract` API sketch (new `ConfigOrigin` enum + `ConfigSource.origin`). No code change beyond the rename.

### m5 — `enabled=false` vs the "no uninstall" non-goal is undefined for the ensure pass

- **Severity:** MINOR
- **Affected:** DOC-2 (`enabled`), DOC-3 §4, spec §62
- **Code-verified:** no
- **The flaw:** no defined ensure behavior for a previously-ensured member now disabled.
- **Failure mode:** orphaned `.mcp.json` entries + project state pointing at a disabled tool, with no cleanup path (no-uninstall).
- **Fix direction:** define `enabled=false` ensure semantics (skip, leave state, `skipped` doctor check); note the no-uninstall tension.
- **Status:** ✅ Resolved
- **Decision:** `enabled = false` ⇒ the ensure pass skips the member entirely (no install/config/start/upgrade); its existing per-project and system state is left in place (the no-uninstall non-goal — the controller never removes an index/palace/`.mcp.json` entry); the member renders a `skipped` doctor check (note: "disabled in manifest"), not `down`/`fail`. Known consequence acknowledged honestly: orphaned `.mcp.json` entries / per-project state from a previously-enabled member remain (no cleanup path, per no-uninstall); manual removal is the user's path.
- **Follow-up:** DOC-2 §3 (`enabled` field semantics) + DOC-3 §4 ("Disabled members are skipped" in the ensure pass).

### m6 — UUC2's truly-vanilla user can't reach the first `tctl` invocation

- **Severity:** MINOR
- **Affected:** DOC-8 §5, DOC-10 §3.3
- **Code-verified:** no
- **The flaw:** guide-and-abort fires only after `tctl` runs, but `tctl` itself needs cargo to install; the pre-`tctl` rust+cargo bootstrap lives only in the DOC-10 harness, not the product flow.
- **Failure mode:** the zero-knowledge persona can't get started.
- **Fix direction:** document the pre-`tctl` bootstrap one-liner (install rust → `cargo install trusty-controller`) as the UUC2 entry point.
- **Status:** ✅ Resolved
- **Decision:** document the pre-`tctl` bootstrap as the explicit UUC2 product on-ramp in DOC-8 — **STEP 0:** install Rust (official rustup one-liner) → `cargo install trusty-controller` → then `tctl install`. The §5 guide-and-abort fires only once `tctl` exists (covering remaining hard deps like `uv`), so the rust→cargo-install step is the genuine zero-knowledge entry point and belongs in the product doc, not only the DOC-10 harness. Consistent with the no-auto-install-toolchains stance (STEP 0 is documented user guidance, not a `tctl` action). DOC-10 §3.3's rustup+uv first step is aligned as the executable mirror of STEP 0.
- **Follow-up:** DOC-8 §1.1 (STEP 0 pre-`tctl` bootstrap one-liner) + §5 (back-ref to STEP 0). DOC-10 §3.3 (note the harness first-step mirrors STEP 0).

### m7 — Controller is `kind=cli` in the manifest yet is a launchd-supervised daemon

- **Severity:** MINOR
- **Affected:** DOC-2 (manifest example), DOC-7 §8, DOC-8 §1.1, DOC-5 §7, DOC-9 §5.2
- **Code-verified:** no
- **The flaw:** the `kind` enum (`daemon`/`cli`) has no slot for a system-only daemon; mislabeling forces name-special-casing in restart/supervision logic.
- **Failure mode:** `kind=="daemon"` branches skip the controller, then docs special-case it by name (the forbidden tool-specific branching).
- **Fix direction:** give the controller `kind=daemon`, or add a "system-only daemon" kind.
- **Status:** ✅ Resolved
- **Decision:** add a new `kind` value `controller` — a supervised, system-only daemon that is the controller itself (launchd/systemd-supervised like a `daemon`, but no project layer like a `cli`, restarted last via self-exit per DOC-9 §8, never an external bootout because a process cannot bootout itself). The controller manifest entry changes from `kind = "cli"` to `kind = "controller"`. Restart/supervision logic now branches on `kind` (the `controller` kind is a supervised system-only daemon, restarted last) rather than special-casing the controller by name; the self-exit specifics remain the controller's legitimate self-recognition (it knows its own member id — the permitted single-self-exclusion from DOC-5 §2/§6 / M5), not per-tool branching. This closes both the `kind = "cli"` mislabel and the forbidden name-special-case.
- **Follow-up:** DOC-2 §3 (kind enum gains `controller` + controller manifest entry → `kind = "controller"`); DOC-3 §1 (controller-kind = system-only supervised daemon layer note); DOC-5 §7 (restart set keys off `kind = "controller"`, not name); DOC-7 §8 + DOC-9 §5.2/§8 (corrected label, no behavior change).

### m8 — UI link-out depends on the `port --json` + `/ui` convention (convention-deep, not contract-deep)

- **Severity:** MINOR
- **Affected:** DOC-7 §1/§4.1
- **Code-verified:** no
- **The flaw:** genericity holds only for tools matching the search/memory convention; a differently-discovered port needs controller code.
- **Failure mode:** low; oversells "mechanical, not per-tool."
- **Fix direction:** acknowledge the convention as a 4th dependency, or fold port-discovery into the contract verb set.
- **Status:** ✅ Resolved
- **Decision:** acknowledge the `/ui` + `port --json` convention as an **explicit bounded dependency**, consistent with M1's bounded tool-class enumeration (DOC-5 §2.2.1 already lists this convention as a tool-class assumption and cross-refs this very item). Link-out is generic **only** for members declaring `port_source = "port_json"` + `path = "/ui"`; the manifest `port_source` field is the **extension point** for a different discovery mechanism — a tool with another mechanism adds a *new* `port_source` value (the discovery branch keys off `port_source`, never the tool's name). That is a **bounded, manifest-declared tool-class assumption, not per-tool controller branching**. So "mechanical, not per-tool" is honestly reframed as **convention-deep, not contract-deep**: mechanical for members following the convention; a new discovery mechanism is a bounded, manifest-declared extension — not "works for any tool whatsoever."
- **Follow-up:** DOC-7 §4.1 (new convention-bounded-genericity note — link-out generic only for `port_source = "port_json"` + `/ui`; `port_source` as the extension point for a different mechanism; cross-ref DOC-5 §2.2.1 / M1's tool-class table; "convention-deep, not contract-deep" framing). No code change. (DOC-5 §2.2.1 already enumerates this convention as a tool-class assumption — no further edit needed there.)

### m9 — DNS-rebind guard is sound for browsers but a local non-browser process can still bounce the stack

- **Severity:** MINOR
- **Affected:** DOC-7 §6, Resolved-Decision-3
- **Code-verified:** no
- **The flaw:** Origin check + `confirmed` closes the browser hole, but any local non-browser caller (other user/malware on a multi-user macOS box) can POST `confirmed:true` (no auth, no per-process identity).
- **Failure mode:** low-probability, high-blast-radius local stack-wide restart/upgrade.
- **Fix direction:** note the residual threat; consider a one-time CLI-minted token (user-owned config file) for mutating endpoints.
- **Status:** ✅ Resolved
- **Decision:** (1) **note the residual threat explicitly** — the Origin + `confirmed` guard defends against browser-driven DNS-rebind/stale-tab attacks, but a local **non-browser** process on a multi-user box can still forge a mutating POST (`confirmed: true` with a forged/absent Origin; no auth, no per-process identity); **loopback trust is the v1 posture** and the single-user-box residual is explicitly accepted. (2) Add an **opt-in capability token** for mutating endpoints: a one-time **CLI-minted token** stored in a **user-owned `0600` config file** that the controller UI reads (same-origin, same user) and includes on mutating POSTs — a process owned by a *different* user, or without read access to that file, cannot mint a valid mutating request, closing the non-browser hole on a shared box. Kept **opt-in / lightweight** for v1: baseline = loopback + Origin + `confirmed`; the `0600` capability token is **defense-in-depth** for multi-user/shared hosts (not a mandatory auth layer — preserves no-auth parity with the existing daemons). **Resolves the §6 question (Resolved Decision 3):** baseline = loopback + Origin + `confirmed`; opt-in = `0600` CLI-minted capability token for multi-user hardening; residual = single-user-box loopback trust, accepted for v1.
- **Follow-up:** DOC-7 §6 (new residual-threat bullet — local non-browser process can forge a mutating POST despite Origin + `confirmed`; loopback trust = v1 posture — plus the opt-in capability-token bullet: CLI-minted, `0600` user-owned file, UI sends it same-origin on mutating POSTs) + Resolved Decision 3 reworded to record the residual + opt-in token + accepted single-user residual. No code change beyond the opt-in token at implementation time.

### m10 — `stack health`/`stack doctor` consistency guarantee is a cross-probe race

- **Severity:** MINOR
- **Affected:** DOC-4 §4
- **Code-verified:** no
- **The flaw:** the two commands run at different times/timeouts; a daemon crashing between sweeps makes them disagree (not a contract violation, a race).
- **Failure mode:** DOC-10's asserted invariant flakes; readers over-trust health and skip doctor.
- **Fix direction:** weaken to "within a single combined probe"; harness asserts over one collection.
- **Status:** ✅ Resolved
- **Decision:** weaken the consistency guarantee to hold **within a single combined probe/collection**. Across two separately-timed invocations (`stack health` then `stack doctor`) a daemon state change between the two sweeps can legitimately make them differ — that is a **race**, not a contract violation. The invariant (health `down` ⇒ doctor's system column `down`) holds within **one collection pass** (a single combined probe that gathers both the health and doctor signals in the same sweep), and DOC-10's harness asserts it over **one collection**, not by diffing two separate CLI runs.
- **Follow-up:** DOC-4 §4 ("Consistency guarantee" reworded — scoped to a single combined probe/collection; cross-invocation differences framed as a race, not a violation; harness asserts over one collection). Implementation follow-up: the combined-probe collection path + the harness assertion over one pass.

### m11 — Effort/scope realism: the "thin coordinator" understates the retrofit

- **Severity:** MINOR
- **Affected:** DOC-6 §2–§3/§7
- **Code-verified:** no
- **The flaw:** net-new `trusty_common::contract` module + `restart`/`version`/read-only-`config` on every tool + the entire trusty-review verb surface + the Python shim + the stateful identity migration (see M15).
- **Failure mode:** timeline/risk under-budgeted; "draw the rest of the owl" steps.
- **Fix direction:** re-scope DOC-6 as "4 tool retrofits + 1 shim + 1 new crate + 1 stateful migration," each its own worktree/PR.
- **Status:** ✅ Resolved
- **Decision:** Option A (cluster) — re-scope DOC-6 §3 as an **honest, enumerated work-breakdown** (DOC-6 §3.1), each item its own worktree/PR: (1) net-new `trusty_common::contract` module + `Dispatcher` (+ the **C3** golden-snapshot conformance test), (2) trusty-search retrofit (+ the **M4** `config`→`tune` split + the §7 project-identity reconciliation), (3) trusty-memory retrofit, (4) trusty-analyze retrofit, (5) trusty-review retrofit (the laggard — net-new `doctor`/`version`/`config`/`restart`), (6) the claude-mpm Python shim (version-coupled, drift-tested), (7) the stateful project-identity migration (re-keys index/palace state; **design deferred to M15** — counted here, not designed). Qualify the "thin coordinator" claim: the *controller* is a thin coordinator (zero per-tool verb-dispatch logic), but the *contract retrofit it depends on* is **substantial** — N discrete PRs across **4 Rust tools + 1 shim + 1 new shared module + 1 stateful migration**. Note that recent decisions added **net-new surface** to this exact retrofit (the **C3** snapshot test, **C5**'s `restart` verb ×4 daemons, **M4**'s `tune`-verb split), so the breakdown reflects the current decided state. M15 is **not** designed here — it is only counted as a work-item.
- **Follow-up:** DOC-6 §3.1 (new enumerated 7-item work-breakdown + the thin-coordinator qualifier + the C3/C5/M4 net-new-surface note) + Remaining-work checklist (mapped onto the 7 items, item 7 design deferred). Cross-ref [M15](#m15--project-identity-migration-orphans-existing-indexpalace-state) (the migration's design). No effort change to the controller itself; the breakdown is **documentation of existing scope**, not new scope.
