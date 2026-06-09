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
| C5 | CRITICAL | DOC-9 "new versions take effect via connection-safe restart" assumes a `restart` verb DOC-6 says is net-new on every tool | DOC-9 §5.1, DOC-6 §2.1–2.4 | — | 🔴 Open |
| M1 | MAJOR | "Zero tool-specific logic" is oversold (relocated, not eliminated) | spec §83; DOC-2 §6, DOC-5 §1/§2.2, DOC-6 §4, DOC-3 §7, DOC-7 | — | 🔴 Open |
| M2 | MAJOR | `stack_version` tuple will rot (no test owner, no CI gate, embedded in the binary) | DOC-2 §4, Resolved-Q5 | — | 🔴 Open |
| M3 | MAJOR | Cross-repo claude-mpm dependency is load-bearing in 4 places but never assessed as one systemic risk | DOC-6 §4, DOC-8 §4/§5, DOC-9 §3.4, DOC-10 §6 | — | 🔴 Open |
| M4 | MAJOR | `config` verb is a breaking CLI change + a hole in the read-only guarantee | DOC-1 `config.data`, DOC-5 §1.2, DOC-6 §2.6, DOC-3 Q3 | ✅ code-verified | 🔴 Open |
| M5 | MAJOR | clap `external_subcommand` passthrough collides with first-class subcommands + controller-as-member recursion | DOC-5 §6/§1.1 | — | 🔴 Open |
| M6 | MAJOR | CLI-as-authority (D1) under-specified for "daemon up but wedged" | DOC-1 D1, `health.data` | — | 🔴 Open |
| M7 | MAJOR | Concurrent "ensure every launch" race on a shared daemon | DOC-3 §4/§6/§8 | — | 🔴 Open |
| M8 | MAJOR | Status-enum reconciliation is ambiguous | DOC-1 D4, DOC-4 §1.1/§4, DOC-3 §9 | — | 🔴 Open |
| M9 | MAJOR | D8 secret redaction is unenforceable | DOC-1 D8, DOC-6 conformance clause 5 | — | 🔴 Open |
| M10 | MAJOR | Timeouts (2s health / 10s doctor) false-fail cold / model-loading daemons | DOC-4 §1.3, Resolved-Q1 | — | 🔴 Open |
| M11 | MAJOR | `state.toml` stack-version tracking can silently desync; UI shows stale "clean" | DOC-9 §4.4, Resolved-Q3, DOC-7 §2.1 | — | 🔴 Open |
| M12 | MAJOR | No auto-rollback + non-transactional installs can wedge a partial tuple | DOC-9 §6/§3.6, Resolved-Q5 | — | 🔴 Open |
| M13 | MAJOR | Self-upgrade abandons in-flight UI op state | DOC-9 §8, DOC-7 §8/§3.2 | — | 🔴 Open |
| M14 | MAJOR | CI tests the wrong paths for the primary target | DOC-10 §4.1/§6b, DOC-8 §6, Resolved-Decision-5/6 | — | 🔴 Open |
| M15 | MAJOR | Project-identity migration orphans existing index/palace state | DOC-6 §7, DOC-3 §8, ADR-0008 | — | 🔴 Open |
| m1 | MINOR | Aggregate exit code undefined for fan-out `stack doctor` | DOC-1 D5, DOC-4 | — | 🔴 Open |
| m2 | MINOR | `pending` is gameable (no stall detection) | DOC-3 §2, DOC-1 `doctor.data`, Resolved-Q5 | — | 🔴 Open |
| m3 | MINOR | Project-scope `fail`/`down` exits 0, masking a real local outage | DOC-4 §2.2/§7, Resolved-Q3 | — | 🔴 Open |
| m4 | MINOR | `scope` vs config-provenance `scope` overload is leaky + stringly-typed | DOC-1 D7 + `config.data` (`ConfigSource.scope: String`) | — | 🔴 Open |
| m5 | MINOR | `enabled=false` vs the "no uninstall" non-goal is undefined for the ensure pass | DOC-2 (`enabled`), DOC-3 §4, spec §62 | — | 🔴 Open |
| m6 | MINOR | UUC2's truly-vanilla user can't reach the first `tctl` invocation | DOC-8 §5, DOC-10 §3.3 | — | 🔴 Open |
| m7 | MINOR | Controller is `kind=cli` in the manifest yet is a launchd-supervised daemon | DOC-2 (manifest example), DOC-7 §8, DOC-8 §1.1, DOC-5 §7, DOC-9 §5.2 | — | 🔴 Open |
| m8 | MINOR | UI link-out depends on the `port --json` + `/ui` convention (convention-deep, not contract-deep) | DOC-7 §1/§4.1 | — | 🔴 Open |
| m9 | MINOR | DNS-rebind guard is sound for browsers but a local non-browser process can still bounce the stack | DOC-7 §6, Resolved-Decision-3 | — | 🔴 Open |
| m10 | MINOR | `stack health`/`stack doctor` consistency guarantee is a cross-probe race | DOC-4 §4 | — | 🔴 Open |
| m11 | MINOR | Effort/scope realism: the "thin coordinator" understates the retrofit | DOC-6 §2–§3/§7 | — | 🔴 Open |

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
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

---

## MAJOR

### M1 — "Zero tool-specific logic" is oversold (relocated, not eliminated)

- **Severity:** MAJOR
- **Affected:** spec §83; DOC-2 §6, DOC-5 §1/§2.2, DOC-6 §4, DOC-3 §7, DOC-7
- **Code-verified:** no
- **The flaw:** hard-coded install-source templates, the claude-mpm shim, `SKIP_UI_BUILD` derivation, `/ui`+`port --json` convention, and `config` precedence are tool-class knowledge in the shipped artifact.
- **Failure mode:** misleads maintainers into thinking any conformant tool "just works."
- **Fix direction:** reword to "zero tool-specific *verb-dispatch* logic" + enumerate the bounded tool-class assumptions.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M2 — `stack_version` tuple will rot (no test owner, no CI gate, embedded in the binary)

- **Severity:** MAJOR
- **Affected:** DOC-2 §4, Resolved-Q5
- **Code-verified:** no
- **The flaw:** no mechanism/owner/CI to materialize-and-test a candidate tuple; per-#343 independent versioning staleness; the BOM is compiled into `tctl` so new pins require a controller release.
- **Failure mode:** "keep current with minimal effort" silently degrades to "current as of last tctl release."
- **Fix direction:** a stack-integration CI matrix that tests candidate tuples; consider decoupling the BOM from the binary (the deferred remote channel).
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M3 — Cross-repo claude-mpm dependency is load-bearing in 4 places but never assessed as one systemic risk

- **Severity:** MAJOR
- **Affected:** DOC-6 §4, DOC-8 §4/§5, DOC-9 §3.4, DOC-10 §6
- **Code-verified:** no
- **The flaw:** uv install + launch hook + output-parsing shim + version pin all couple to an external evolving repo; the shim parses *human* `mpm-doctor` text while install floats to *latest* (unpinned) — a contradiction; best-effort parsing degrades silently.
- **Failure mode:** an upstream cosmetic change misclassifies orchestrator health and the every-PR CI (frozen pin) won't catch it.
- **Fix direction:** a single cross-cutting "claude-mpm external-dependency risk" section + a shim contract-test that fails loudly on drift; reconcile unpinned-install vs version-coupled-parser.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M4 — `config` verb is a breaking CLI change + a hole in the read-only guarantee

- **Severity:** MAJOR
- **Affected:** DOC-1 `config.data`, DOC-5 §1.2, DOC-6 §2.6, DOC-3 Q3
- **Code-verified:** yes (`crates/trusty-search/src/main.rs` — existing `config get/set` mutates live daemon limits)
- **The flaw:** overloading the contract's read-only `config` onto a tool that ships a mutating `config set` is semver-relevant; passthrough (`tctl <tool> config set`) forwards mutations through a "read-only config" controller.
- **Failure mode:** a UI/agent mutates via passthrough despite the read-only posture.
- **Fix direction:** rename the mutating verb (e.g. `tune`/`limits`); define passthrough's stance on mutating subcommands.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M5 — clap `external_subcommand` passthrough collides with first-class subcommands + controller-as-member recursion

- **Severity:** MAJOR
- **Affected:** DOC-5 §6/§1.1
- **Code-verified:** no
- **The flaw:** `tctl trusty-controller restart` routes passthrough → `tctl restart` (self); global flags after the verb get swallowed into forwarded args; "sugar over passthrough" under-specified.
- **Failure mode:** recursive/ambiguous dispatch requiring name-special-casing (the thing the design forbids).
- **Fix direction:** document controller-as-member exclusion + flag-position rule, or use explicit `tool/rest` positionals.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M6 — CLI-as-authority (D1) under-specified for "daemon up but wedged"

- **Severity:** MAJOR
- **Affected:** DOC-1 D1, `health.data`
- **Code-verified:** no
- **The flaw:** no timeout/`unknown` state — a hung daemon that connects but never responds is neither cleanly `down` nor `running`.
- **Failure mode:** `tctl stack doctor` hangs or misreports a wedged daemon as running (PID lockfile exists).
- **Fix direction:** mandatory per-verb invocation timeout + a `degraded`/`unknown` mapping for slow/no-response.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M7 — Concurrent "ensure every launch" race on a shared daemon

- **Severity:** MAJOR
- **Affected:** DOC-3 §4/§6/§8
- **Code-verified:** no
- **The flaw:** two simultaneous project launches both see "daemon down" and both `start`; no system-scope lock (TOCTOU).
- **Failure mode:** double-start, port-bind/lock contention, install/upgrade triggered mid-ensure.
- **Fix direction:** system-scope advisory lock around the system-rung ensure; define loser behavior.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M8 — Status-enum reconciliation is ambiguous

- **Severity:** MAJOR
- **Affected:** DOC-1 D4, DOC-4 §1.1/§4, DOC-3 §9
- **Code-verified:** no
- **The flaw:** `health{running|degraded|down}` vs `doctor{ok|warn|fail|pending|skipped}` have no defined total order; `stack health` and `stack doctor` can disagree in direction.
- **Failure mode:** contradictory verdicts / CI exit-code ambiguity (both warn and degraded → exit 2).
- **Fix direction:** define one total order across the union; frame health/doctor as fast/deep views of one lattice.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M9 — D8 secret redaction is unenforceable

- **Severity:** MAJOR
- **Affected:** DOC-1 D8, DOC-6 conformance clause 5
- **Code-verified:** no
- **The flaw:** tools self-report already-redacted JSON; controller can't verify; helper is opt-in; no CI gate.
- **Failure mode:** one buggy tool (e.g. the shim) leaks a key/credential into the web UI.
- **Fix direction:** defense-in-depth controller-side redaction pass over envelope strings before render.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M10 — Timeouts (2s health / 10s doctor) false-fail cold / model-loading daemons

- **Severity:** MAJOR
- **Affected:** DOC-4 §1.3, Resolved-Q1
- **Code-verified:** no
- **The flaw:** ONNX model load, cold start, or graceful-shutdown window blow the fixed timeouts → synthesized `down`.
- **Failure mode:** `tctl stack doctor` CI gate false-fails a healthy-but-slow cold stack; a gracefully-restarting daemon reads as down.
- **Fix direction:** cold-start budget / warmup ping; ship per-member timeout overrides day one.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M11 — `state.toml` stack-version tracking can silently desync; UI shows stale "clean"

- **Severity:** MAJOR
- **Affected:** DOC-9 §4.4, Resolved-Q3, DOC-7 §2.1
- **Code-verified:** no
- **The flaw:** crash mid-upgrade, no locking (UI polls every 10s while CLI upgrades), lazy reconcile only on `tctl status`.
- **Failure mode:** UI shows "on 2026.07-1 ✓" while two members are still old.
- **Fix direction:** derive displayed version live from `version --json` (state.toml = cache only); write atomically + advisory-locked.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M12 — No auto-rollback + non-transactional installs can wedge a partial tuple

- **Severity:** MAJOR
- **Affected:** DOC-9 §6/§3.6, Resolved-Q5
- **Code-verified:** no
- **The flaw:** per-member health-gate defends the single-member case, not the cross-member case (new-search + old-analyze = untested tuple; no tuple-level gate).
- **Failure mode:** drifted, untested, partially-new stack; recovery is manual `cargo install @old` (defeats zero-knowledge).
- **Fix direction:** gate dependent restarts on the dependency's verify-after; make "drifted/partial-tuple" a first-class outcome with one-command pin-back.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M13 — Self-upgrade abandons in-flight UI op state

- **Severity:** MAJOR
- **Affected:** DOC-9 §8, DOC-7 §8/§3.2
- **Code-verified:** no
- **The flaw:** the SSE replay buffer lives in the dying process's memory; respawn has no replay for the pre-restart op.
- **Failure mode:** browser sees a gap with no `complete` event; UI stuck "reconnecting…" for an op that finished.
- **Fix direction:** persist op terminal-state, or make self-upgrade CLI-only in v1.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M14 — CI tests the wrong paths for the primary target

- **Severity:** MAJOR
- **Affected:** DOC-10 §4.1/§6b, DOC-8 §6, Resolved-Decision-5/6
- **Code-verified:** no
- **The flaw:** every-PR Linux leg uses foreground `serve` (production = systemd-user, tested by no one per-PR); macOS is "primary" but its gate (incl. the cdhash SIGKILL trap) runs only nightly.
- **Failure mode:** supervision/restart regressions and the macOS `cp`-into-PATH SIGKILL regress green and ship to nightly discovery.
- **Fix direction:** minimal per-PR macOS smoke (install + health + cdhash assertion) + a systemd-user leg per-PR.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### M15 — Project-identity migration orphans existing index/palace state

- **Severity:** MAJOR
- **Affected:** DOC-6 §7, DOC-3 §8, ADR-0008
- **Code-verified:** no
- **The flaw:** the basename→slug flip re-keys every existing index/palace; DOC-6 treats it as a refactor, not a stateful data migration; both id forms are currently live in the daemon registry for the same root.
- **Failure mode:** first `tctl ensure` re-indexes everything from scratch (multi-minute "pending", storage doubling).
- **Fix direction:** explicit alias/re-key migration step in the trusty-search retrofit.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

---

## MINOR

### m1 — Aggregate exit code undefined for fan-out `stack doctor`

- **Severity:** MINOR
- **Affected:** DOC-1 D5, DOC-4
- **Code-verified:** no
- **The flaw:** single-member exit codes defined, but the controller's own aggregate code across N members (mix of 3/1/2/0) isn't.
- **Failure mode:** CI scripts get inconsistent signals; contract-incompatible (install problem) indistinguishable from fail (runtime).
- **Fix direction:** define aggregate precedence (e.g. `3 > 1 > 2 > 0`) in DOC-1 D5 / DOC-4.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m2 — `pending` is gameable (no stall detection)

- **Severity:** MINOR
- **Affected:** DOC-3 §2, DOC-1 `doctor.data`, Resolved-Q5
- **Code-verified:** no
- **The flaw:** freshness is tool-self-reported and `pending` never worsens the rollup; a crash-looping indexer can report `pending` forever.
- **Failure mode:** "usable now, ready in ~Ns" becomes permanently pending while doctor stays green-ish.
- **Fix direction:** require `pending` to carry `pending_since`/`progress_pct`; controller escalates a long-stalled `pending` to `warn` by elapsed time (without inspecting the index).
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m3 — Project-scope `fail`/`down` exits 0, masking a real local outage

- **Severity:** MINOR
- **Affected:** DOC-4 §2.2/§7, Resolved-Q3
- **Code-verified:** no
- **The flaw:** a genuine project `fail` (not `pending`) keeps exit 0 (system-track-driven), same code as a healthy stack.
- **Failure mode:** a corrupt index in this checkout never blocks a scripted launch; only signal is a buried glyph.
- **Fix direction:** opt-in `--fail-on-project` or a louder summary; stop folding `fail` into the `pending`-is-not-broken framing.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m4 — `scope` vs config-provenance `scope` overload is leaky + stringly-typed

- **Severity:** MINOR
- **Affected:** DOC-1 D7 + `config.data` (`ConfigSource.scope: String`)
- **Code-verified:** no
- **The flaw:** two `scope` fields with overlapping-but-different value sets in one JSON doc; provenance is an untyped string (`env` can't reuse the `Scope` enum).
- **Failure mode:** authors conflate them; controller can't catch typos.
- **Fix direction:** rename provenance to `origin` (`{env|project|system}`) with its own enum; one `scope` axis.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m5 — `enabled=false` vs the "no uninstall" non-goal is undefined for the ensure pass

- **Severity:** MINOR
- **Affected:** DOC-2 (`enabled`), DOC-3 §4, spec §62
- **Code-verified:** no
- **The flaw:** no defined ensure behavior for a previously-ensured member now disabled.
- **Failure mode:** orphaned `.mcp.json` entries + project state pointing at a disabled tool, with no cleanup path (no-uninstall).
- **Fix direction:** define `enabled=false` ensure semantics (skip, leave state, `skipped` doctor check); note the no-uninstall tension.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m6 — UUC2's truly-vanilla user can't reach the first `tctl` invocation

- **Severity:** MINOR
- **Affected:** DOC-8 §5, DOC-10 §3.3
- **Code-verified:** no
- **The flaw:** guide-and-abort fires only after `tctl` runs, but `tctl` itself needs cargo to install; the pre-`tctl` rust+cargo bootstrap lives only in the DOC-10 harness, not the product flow.
- **Failure mode:** the zero-knowledge persona can't get started.
- **Fix direction:** document the pre-`tctl` bootstrap one-liner (install rust → `cargo install trusty-controller`) as the UUC2 entry point.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m7 — Controller is `kind=cli` in the manifest yet is a launchd-supervised daemon

- **Severity:** MINOR
- **Affected:** DOC-2 (manifest example), DOC-7 §8, DOC-8 §1.1, DOC-5 §7, DOC-9 §5.2
- **Code-verified:** no
- **The flaw:** the `kind` enum (`daemon`/`cli`) has no slot for a system-only daemon; mislabeling forces name-special-casing in restart/supervision logic.
- **Failure mode:** `kind=="daemon"` branches skip the controller, then docs special-case it by name (the forbidden tool-specific branching).
- **Fix direction:** give the controller `kind=daemon`, or add a "system-only daemon" kind.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m8 — UI link-out depends on the `port --json` + `/ui` convention (convention-deep, not contract-deep)

- **Severity:** MINOR
- **Affected:** DOC-7 §1/§4.1
- **Code-verified:** no
- **The flaw:** genericity holds only for tools matching the search/memory convention; a differently-discovered port needs controller code.
- **Failure mode:** low; oversells "mechanical, not per-tool."
- **Fix direction:** acknowledge the convention as a 4th dependency, or fold port-discovery into the contract verb set.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m9 — DNS-rebind guard is sound for browsers but a local non-browser process can still bounce the stack

- **Severity:** MINOR
- **Affected:** DOC-7 §6, Resolved-Decision-3
- **Code-verified:** no
- **The flaw:** Origin check + `confirmed` closes the browser hole, but any local non-browser caller (other user/malware on a multi-user macOS box) can POST `confirmed:true` (no auth, no per-process identity).
- **Failure mode:** low-probability, high-blast-radius local stack-wide restart/upgrade.
- **Fix direction:** note the residual threat; consider a one-time CLI-minted token (user-owned config file) for mutating endpoints.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m10 — `stack health`/`stack doctor` consistency guarantee is a cross-probe race

- **Severity:** MINOR
- **Affected:** DOC-4 §4
- **Code-verified:** no
- **The flaw:** the two commands run at different times/timeouts; a daemon crashing between sweeps makes them disagree (not a contract violation, a race).
- **Failure mode:** DOC-10's asserted invariant flakes; readers over-trust health and skip doctor.
- **Fix direction:** weaken to "within a single combined probe"; harness asserts over one collection.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_

### m11 — Effort/scope realism: the "thin coordinator" understates the retrofit

- **Severity:** MINOR
- **Affected:** DOC-6 §2–§3/§7
- **Code-verified:** no
- **The flaw:** net-new `trusty_common::contract` module + `restart`/`version`/read-only-`config` on every tool + the entire trusty-review verb surface + the Python shim + the stateful identity migration (see M15).
- **Failure mode:** timeline/risk under-budgeted; "draw the rest of the owl" steps.
- **Fix direction:** re-scope DOC-6 as "4 tool retrofits + 1 shim + 1 new crate + 1 stateful migration," each its own worktree/PR.
- **Status:** 🔴 Open
- **Decision:** _(owner fills this in)_
- **Follow-up:** _(doc/ADR edits to make once decided)_
