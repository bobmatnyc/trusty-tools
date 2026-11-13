# DOC-91 — Settable technical level for Architect owner-facing text

**Status:** Draft (owner-accepted 2026-11-11)
**Subsystem:** Architect — `python/trusty-architect` (technical level reader, hook line, settings selector, level lint), shipped skills under `content/instructions/architect/`, `trusty-architect` dashboard store
**Owner:** owner
**Last-updated:** 2026-11-12
**Spec ID:** `SPEC-TECHLEVEL-01~draft` … `SPEC-TECHLEVEL-08~draft` (DOC-91)
**DOC-N claim:** `DOC-91`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). The catalog's next-free note on `origin/main` at 2026-11-12 (`f126f298d9`) named `DOC-91`; no file or open PR claims it.
**Issue:** [#11064](https://github.com/bobmatnyc/trusty-tools/issues/11064). Copied from the spec comment on that issue (2026-11-11 17:48Z); sections 1-7 keep its text. Section 8 records the framework port.

Goal ar-G05, task ar-T16. Requested by Bob, 2026-11-11 17:29Z.
Owner ruling (Bob, 2026-11-11 17:47Z, decision Deb): "Accept all four" — six levels, Bob = pm with detail on request, default pm, violations warn only, applies to the morning brief and Slack.
Bob's words: "in general, we need to elevate the language. I'm capable of understanding most things, but don't want to get into technical details unless I have to. We should have settable technical levels for the architect. Non-Technical, Vibe-Coder, PM, Engineer, Senior Engineer, CTO. Scope/spec/ticket"
Requirement ids are `TL-n`. "MUST" marks a testable requirement.

## 1. Scope {#SPEC-TECHLEVEL-01~draft}

**ID:** SPEC-TECHLEVEL-01~draft

In scope:
- TL-1. A per-user setting `technical_level` with six values: `non-technical`, `vibe-coder`, `pm`, `engineer`, `senior-engineer`, `cto`.
- TL-2. The level governs text a human reads: chat replies, dashboard D/A/T/Q items (title, detail, options), owner briefs, the morning brief, Slack delivery, and the interactive questions session.
- TL-3. A per-item "more detail" request raises depth for that item only. It does not change the setting.

Out of scope:
- Relays to PMs and agents. They stay technical and complete.
- Ledgers and records: `local/actions.md`, `local/state.md`, the IL, memory drawers, rulings. They stay technical.
- Linked briefs. A brief holds the full technical detail that the level removes from the item.
- The voice guide (`docs/architect/voice.md`). It sets tone; this spec sets altitude. They compose.

Verified facts:
- Owner settings live in `local/owner-queue/meta/settings.json` (keys seen: `user_state`, `tz`, `sleep_start`). Verified.
- `scripts/user_state.py` reads `meta/settings` through `dashboard_store`. `scripts/pending_hook_dedupe.py` prints `USER STATE: <state>` as hook context. Verified.
- `local/users/registry.json` exists, is `enabled: false` and holds no users. Verified.
- Prose rules exist in `ARCHITECT.md` ("Communication") and `docs/architect/voice.md`. No altitude control exists. Verified by search of both.
- No duplicate issue exists in bobmatnyc/trusty-tools (search run 2026-11-11). Verified.

## 2. The six levels {#SPEC-TECHLEVEL-02~draft}

**ID:** SPEC-TECHLEVEL-02~draft

Common rule: every level states the same decision and the same recommendation. Only vocabulary, evidence and length change. A level never hides a risk; it restates the risk in that level's terms.

### non-technical
- Reader: a business owner who does not build software.
- Vocabulary: everyday words. No code terms. Say "the system" or "the website", not "server slot" or "config".
- Omitted: all paths, line numbers, SHAs, flags, tool and job names. Moved to a linked brief.
- Framing: what changes for the business, what it costs in time or money, what goes wrong if we pick wrong.
- Length: 2 to 3 sentences, 2 options at most, each one sentence.

### vibe-coder
- Reader: builds with AI tools, knows what an app, a deploy and a repo are, not how they work inside.
- Vocabulary: product words (app, deploy, test, repo, bug). Names a tool only when the owner uses it.
- Omitted: line numbers, SHAs, internals of CI, flag names. Moved to a linked brief.
- Framing: what the user sees, how hard to undo, whether a deploy is needed.
- Length: 3 to 4 sentences, options in one sentence each.

### pm
- Reader: runs delivery. Knows scope, risk, schedule, dependencies, environments. Not the code.
- Vocabulary: project terms (environment, release, rollback, owner, blocker). Component names allowed, code identifiers not.
- Omitted: file paths, line numbers, SHAs, flag and job names. Issue and PR links stay.
- Framing: outcome, cost (effort, delay), risk, reversibility, what is needed from the owner.
- Length: 4 to 6 sentences.

### engineer
- Reader: writes and reviews code in this stack.
- Vocabulary: full technical terms, function and module names, issue and PR links.
- Omitted: raw logs and long diffs. Paths appear as module names or short relative paths. SHAs only when they decide the question.
- Framing: behaviour, root cause, trade-off between options, test evidence.
- Length: 6 to 10 sentences, with a short evidence list.

### senior-engineer
- Reader: owns design across components.
- Vocabulary: everything in engineer, plus failure modes, invariants, contracts, concurrency and migration concerns.
- Omitted: only noise. Path and line references included. Blast radius stated.
- Framing: design trade-offs, failure modes, compatibility, rollout and rollback plan.
- Length: as long as the decision needs; evidence by `file:line`.

### cto
- Reader: accountable for the system and the team, decides on risk and investment, reads code on request.
- Vocabulary: technical terms are allowed but used for the decision, not the mechanism. Names the component and the contract at stake.
- Omitted: line numbers, SHAs and job names. They sit one click away in the linked brief.
- Framing: risk to production and customers, cost, reversibility, precedent for other services, and the Architect's recommendation.
- Length: 4 to 8 sentences.

### Default and recommendation
- Recommended default for a new user: `pm`. It is safe for readers of unknown skill and keeps decisions actionable. Assumed.
- Recommended level for Bob: `pm` as the setting, with detail on request. Bob describes himself as a CTO who wants no technical detail unless needed. The wording fits `pm` altitude with `cto` as the alternative. This is a recommendation, not a fact. Bob decides (question 1).
- Unset or unknown value: use `pm` (fail to the default, never to `senior-engineer`).

## 3. Worked example {#SPEC-TECHLEVEL-03~draft}

**ID:** SPEC-TECHLEVEL-03~draft

Source: "When a server slot cannot tell whether it is the live one, should it keep syncing to Notion or stop? Today: if the slot-role line holds anything except exactly 'idle' (a typo, a blank), the slot counts as live. Missing role file counts as live (first-deploy contract, config.py:1994-2045). Options: A keep; B only 'active' counts as live, other values idle with a warning, missing file unchanged; C = B plus missing file counts as idle in prod, needs deploy.sh check."

**non-technical**
Two copies of the system run, and only one should send updates to Notion. Today, if the copy is unsure which one it is, it sends anyway. That can cause duplicate or conflicting updates. Recommended: make an unsure copy stay quiet and flag it (B). Choose A to leave it, or C for the strictest rule, which needs extra deploy work.

**vibe-coder**
We run two copies of the app, and only the live one should sync to Notion. Right now a copy that cannot tell which one it is syncs anyway, so a typo in its settings file makes it act live. B: only an explicit "active" counts as live, anything else stays idle and logs a warning. C: B plus a missing settings file means idle in production, which needs a deploy-script check. Recommended: B.

**pm**
Decision: what a server copy does when it cannot tell whether it is the live one. Today it assumes live and keeps syncing to Notion, so a typo or blank setting can cause two copies to write at once. A keeps that. B treats only an explicit "active" as live and warns otherwise; low effort, no deploy change. C also treats a missing setting as idle in production; safest, but it needs a deploy check first and touches the first-deploy path. Recommended: B now, C as a follow-up.

**engineer**
Slot role resolution fails open. Any value except exactly `idle` counts as live, and a missing role file counts as live (first-deploy contract, `config.py`). A typo or blank therefore runs the Notion sync on both slots. A: keep. B: only `active` is live, other values idle plus a warning, missing file unchanged. C: B plus missing file idle in prod, which requires a `deploy.sh` check so first deploy still goes live. Recommended: B; C needs the deploy check tested first.

**senior-engineer**
Slot role resolution is fail-open (`config.py:1994-2045`): a value other than exactly `idle`, including a typo or blank, resolves to live; a missing role file resolves to live by the first-deploy contract. Failure mode: two live slots run the Notion sync concurrently, giving duplicate or conflicting writes. A: no change, keeps the failure mode. B: `active` is the only live value, others idle with a warning, missing file stays live; closes the typo case, leaves the missing-file case. C: B plus missing file idle in prod; closes both but breaks first deploy unless `deploy.sh` writes the role file before start, so rollout needs that check and a rollback path. Recommended: B now, C after the deploy check.

**cto**
Risk: a server copy that is unsure whether it is live assumes it is, so a small config typo can make two copies write to Notion at once. Cost: B is small and needs no deploy change; C closes the remaining gap but needs a deploy-script check and touches first-deploy behaviour. A leaves the risk open. Recommended: B now, C once the deploy check exists. Component: the slot-role contract in the config loader; line-level detail is in the linked brief.

## 4. Mechanism {#SPEC-TECHLEVEL-04~draft}

**ID:** SPEC-TECHLEVEL-04~draft

Storage:
- TL-4. Bob's level: key `technical_level` in `local/owner-queue/meta/settings.json`, set in the dashboard settings pane beside user state. Verified location.
- TL-5. Secondary users: field `technical_level` per user in `local/users/registry.json` and in the profile `local/users/<name>.md`. The registry is empty today. Verified.
- TL-6. Per-item override: optional field `level_override` on a dashboard item, set by the "more detail" request, cleared when the item closes.

Reading each pass:
- TL-7. The hook prints `TECH LEVEL: <level>` next to `USER STATE:` in `scripts/pending_hook_dedupe.py`, same dedupe and reminder timing as user state. This costs one short line per reminder window. Recommended over reading the file each pass, because the hook already reaches every prompt. Assumed design; the spec leaves the final choice to the engineer.
- TL-8. When the speaker is a secondary user, the level comes from their registry entry, never from Bob's setting.

"Detail on request":
- TL-9. Phrases such as "more detail", "show me the code view", "engineer view" on one item make the Architect rewrite that item once at the next level up (or the named level) and set `level_override`. The setting stays unchanged.
- TL-10. Every item at a level below `senior-engineer` carries a link to a brief that holds the full technical text. The brief is the source of truth, written at `engineer` level or higher.

Files to change (instance, architect repo):
- `.claude/skills/architect-operating-rules/SKILL.md`: add the level rule and the six definitions by reference to this spec.
- `.claude/skills/architect-dashboard/SKILL.md` and `instance-dashboard/SKILL.md`: item title, detail and option text follow the level; add `level_override`.
- `.claude/skills/architect-brief-writing/SKILL.md`: owner brief summary at the level; body stays full technical.
- `.claude/skills/architect-strategy/SKILL.md` and `instance-slack-delivery/SKILL.md`: morning brief and Slack text follow the level.
- `.claude/skills/architect-secondary-users/SKILL.md`: level per user.
- `templates/architect-questions-brief.md`: interactive question text follows the level.
- `scripts/user_state.py` (or new `scripts/tech_level.py`): read and validate the level, default `pm`.
- `scripts/pending_hook_dedupe.py`: print the `TECH LEVEL:` line.
- `templates/dashboard/index.html` and the settings schema (`scripts/dashboard_schema.py`): selector in the settings pane.
- `local/user.md`: record Bob's chosen level once he decides.
- `AGENTS.md` / `ARCHITECT.md`: one rule line pointing to the skill.

Deterministic check:
- TL-11. New script `scripts/level_lint.py` scans the text fields of a dashboard item at write time. At `non-technical`, `vibe-coder` and `pm` it flags file paths, `file:line` pairs, 7 to 40 character hex SHAs, backticked identifiers, CLI flags (`--x`) and CI job names. Issue and PR links are allowed at `pm` and above.
- TL-12. The lint covers format only. It cannot judge vocabulary. A sampled review by the Architect covers the rest. Assumed limit.
- TL-13. The lint runs from `dashboard-db.py` on item create and update, and warns (does not block) in the first release.

## 5. Acceptance criteria {#SPEC-TECHLEVEL-05~draft}

**ID:** SPEC-TECHLEVEL-05~draft

1. `meta/settings.json` accepts `technical_level` with exactly the six values; any other value is rejected and the default `pm` applies.
2. The dashboard settings pane shows a selector for the level and saves it.
3. The hook context includes `TECH LEVEL: <level>` on the first prompt after a change and on the same reminder timing as `USER STATE`.
4. A new D item at `pm` level contains no file path, `file:line`, SHA, flag or job name; `level_lint.py` reports zero findings on it.
5. The same item at `senior-engineer` contains `file:line` evidence.
6. A "more detail" request on one item rewrites only that item and leaves `technical_level` unchanged.
7. Every item below `senior-engineer` links a brief that contains the full technical text.
8. The morning brief and Slack delivery for Bob follow his level.
9. A secondary user's replies use their registry level, not Bob's.
10. Relays to PMs, `local/actions.md`, the IL and memory drawers are unchanged by the setting (diff test on a sample pass).
11. `level_lint.py` has unit tests for each flagged pattern and for the allowed issue link, and fails when a path is injected at `pm`.
12. The six definitions in the skill match this spec; a test checks that each level name appears in the skill and the settings schema.

## 6. Open questions for the owner {#SPEC-TECHLEVEL-06~draft}

**ID:** SPEC-TECHLEVEL-06~draft

1. Which level should be your setting?
   - A. PM, detail on request (Recommended)
   - B. CTO
   - C. Vibe-coder
2. What should the default be for a new or unset user?
   - A. PM (Recommended)
   - B. Non-technical
   - C. Engineer
3. How should a violation be handled in the first release?
   - A. Warn and log, never block (Recommended)
   - B. Block the item until it is rewritten
4. Should the level also apply to the morning brief and Slack messages?
   - A. Yes, same level everywhere (Recommended)
   - B. Dashboard and chat only

## 7. Split {#SPEC-TECHLEVEL-07~draft}

**ID:** SPEC-TECHLEVEL-07~draft

Instance work (architect repo, Python first, prototype):
- Skill and template edits listed in section 4.
- `scripts/tech_level.py`, the hook line, the settings pane selector, `scripts/level_lint.py` and tests.
- Record Bob's choice in `local/user.md` and the rule line in `AGENTS.md`.

Framework work (trusty-tools):
- Move the six definitions and the rule line into `ARCHITECT.md` and the framework skills.
- Ship the settings schema field and selector in the shipped dashboard.
- Port `level_lint.py` to Rust under the Rust-port rule once the Python prototype settles (`local/rust-port.md`).
- Per-user level in the framework's secondary-user registry schema.

## 8. Framework port {#SPEC-TECHLEVEL-08~draft}

**ID:** SPEC-TECHLEVEL-08~draft

Rulings for the port (PM, 2026-11-12):

- (a) An invalid write of `technical_level` or `level_override` is refused; an unknown value already stored reads as `pm`.
- (d) A secondary user gets their own level, and replies to them use it (TL-8, acceptance criterion 9). A prompt marked as a secondary user's never takes the owner's level.
- Relays and records are unchanged by the level.

What shipped, in three pull requests under #11064:

| Part | Files |
|---|---|
| Storage, reader, hook line (TL-1, TL-4 to TL-8) | `scripts/technical_level.py`, `scripts/pending_hook_dedupe.py`, `scripts/dashboard_store.py`, the `meta-settings` and `items` schemas, `templates/local/user.md` (all under `python/trusty-architect/`); the Rust store refusal in `crates/trusty-architect/src/dashboard/store/level.rs` |
| Selector and lint (TL-11 to TL-13) | the settings pane in `templates/dashboard/index.html`, `scripts/level_lint.py`, `scripts/dashboard-db.py` |
| Prompts and skills (TL-2, TL-3, TL-9, TL-10) | `content/instructions/architect/architect.md` and the skills `architect-operating-rules`, `architect-dashboard` and `architect-strategy` (each with its `python/trusty-architect/skills/` source) |

The hook line reads `TECH LEVEL: <level> (<rule>)`, where `<rule>` is a one-line summary of the level. The skills carry the full definitions of section 2.

Differences from section 4:

- TL-6. The store refuses `level_override: null`. An item's override is cleared by reading the item and writing it back with `dashboard-db.py set` and the `level_override` key removed, never by `update` with a null value.
- Files named in section 4 that this repository does not carry: the instance skills `instance-dashboard`, `instance-slack-delivery`, `architect-brief-writing` and `architect-secondary-users`, and the template `templates/architect-questions-brief.md`. The level rules for the brief summary, Slack and secondary users sit in `architect-operating-rules` and `architect-strategy` instead. An instance that adds those files applies the same rules there.
