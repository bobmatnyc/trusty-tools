// Presentation helpers for the coding-delegation style selector (#4353;
// spec DOC-62 `docs/specs/DOC-62-style-modes-coding-delegation.md`).
//
// Why: DOC-62 makes two things true at once that a naive selector gets wrong.
// First, §5.4: a caller-supplied style is a CEILING REQUEST the callee may
// raise but never lower, so the style a user picks is frequently not the style
// that runs — today ALWAYS not, since SM-9 raises `vibe` to `engineer` while
// #2596 is open and the coding lane's own floor raises `hack` to `vibe` first.
// A control that rendered its own selected value would therefore be stating
// something false; OQ-6's answer is to render the EFFECTIVE style and its
// resolution path instead. Second, OQ-3: the style value `engineer` collides
// with the tcode `engineer` SUB-AGENT role, and a user who reads
// `style = engineer` as "delegate to the engineer agent" has read it as
// something epic #4345 decision 2 forbids. DOC-62's recommendation is to keep
// the wire value and disambiguate in the GUI label — so the disambiguation
// lives here, in one place, rather than being re-typed at each render site
// where it could quietly be dropped. That recommendation is a RECOMMENDATION:
// OQ-3 is not formally ratified.
//
// What: pure lookups over the server-computed payload — labels, one-line
// meanings, and the human sentences for a resolution's source and escalations.
// NOTHING here resolves a style. Precedence, the §5.4 ceiling and the SM-9
// fail-safe are computed server-side by the real `ResolvedStyle::resolve` and
// arrive on `SubagentCoding.resolutions`; re-deriving any of it client-side
// would be the second copy of a gate that `agent_subagents.rs`'s honesty rules
// exist to forbid, and would silently go stale the day #2596 lands.
//
// Test: `executionStyle.test.ts`.

import type {
  ExecutionStyle,
  StyleResolution,
  StyleResolutionRow,
  SubagentCoding,
} from './agentConfig';

/** How one style is presented: a disambiguated label plus what it means. */
export interface StylePresentation {
  /** The GUI label. Never the bare wire value for `engineer` — see OQ-3. */
  label: string;
  /** One line on the ceremony level, from DOC-62 §1's table. */
  meaning: string;
}

/**
 * Why: DOC-62 §1 fixes the three names to the already-ratified Execution
 * Patterns tiers, and OQ-3's recommended mitigation for the `engineer`
 * collision is presentational — which makes it "easy to lose" (the spec's own
 * words). Keeping the mitigation in a single exported table is what stops it
 * being lost: a render site cannot accidentally print the raw wire value,
 * because the raw value is not what it is handed.
 * What: label + meaning per style. `engineer`'s label says "full loop" and its
 * meaning states explicitly that it is a ceremony level and not the tcode
 * `engineer` sub-agent, which is the whole point of the entry.
 * Test: `styleLabel_disambiguates_engineer_from_the_subagent_role`,
 * `stylePresentation_covers_every_style`.
 */
export const STYLE_PRESENTATION: Record<ExecutionStyle, StylePresentation> = {
  hack: {
    label: 'Hack — quick ops',
    meaning:
      'Trivial: a direct answer or a couple of reads, with no engineering ceremony at all.',
  },
  vibe: {
    label: 'Vibe — quick coding',
    meaning:
      'A small, self-contained change: no full test suite, no spec, no tickets. The tier itself is not implemented yet (#2596).',
  },
  engineer: {
    label: 'Engineer — full loop',
    meaning:
      'The full lifecycle: spec → issue → worktree → branch → PR → review → merge → CI. This is a CEREMONY LEVEL, not the tcode "engineer" sub-agent — coding work is only ever delegated to the coding PM.',
  },
};

/** The disambiguated GUI label for a style. */
export function styleLabel(style: ExecutionStyle): string {
  return STYLE_PRESENTATION[style].label;
}

/**
 * Why: the resolution path (DOC-62 §3.4) is only useful if a reader can tell
 * WHICH level supplied the value — "your `agent.toml` was read" and "nothing
 * supplied one, so the built-in applied" are different answers to the same
 * user question, and both look identical if only the value is shown.
 * What: a sentence fragment naming the precedence level.
 * Test: `sourceSentence_names_each_precedence_level`.
 */
export function sourceSentence(source: StyleResolution['source']): string {
  switch (source) {
    case 'caller':
      return 'requested on this delegation';
    case 'config':
      return 'from this agent’s [subagents] default_style';
    case 'built-in':
      return 'the built-in default (nothing supplied one)';
  }
}

/**
 * Why: DOC-62 §3.4 requires "an explicit statement when the effective style
 * differs from the requested style, with the reason". `callee-floor` and
 * `tier-unimplemented` are wire tags; a user needs the reason, and SM-9 in
 * particular must not read as a bug — `vibe` running `engineer` is the
 * specified, deliberate fail-safe, and saying so is what stops it being
 * reported as a defect.
 * What: one sentence per escalation reason. Unknown tags (a newer sidecar)
 * degrade to a neutral statement rather than being dropped: an unexplained
 * raise is still a raise the user is entitled to see.
 * Test: `escalationSentence_explains_the_two_specified_reasons`,
 * `escalationSentence_degrades_on_an_unknown_reason`.
 */
export function escalationSentence(reason: string): string {
  switch (reason) {
    case 'callee-floor':
      return 'The coding lane enforces a higher floor: a delegation addressed to the coding PM is by definition asking for a code change, so it can never run below that floor (DOC-62 §5.4).';
    case 'tier-unimplemented':
      return 'That tier has no implementation yet, so the next higher one ran and said so, rather than silently accepting the request and running less (DOC-62 SM-9, #2596).';
    default:
      return `Raised for an unrecognised reason reported by the server (${reason}).`;
  }
}

/**
 * Why: the selector's controls and the payload's rows must be matched by the
 * `caller` key, not by index — the server owns the row order and a positional
 * lookup would silently mis-attribute a resolution to the wrong control if that
 * order ever changed. A missing row is returned as `null` so the caller renders
 * "unreported" rather than a fabricated resolution.
 * What: the row whose `caller` equals `caller` (`null` = the no-override row).
 * Test: `resolutionFor_matches_by_caller_not_by_position`,
 * `resolutionFor_returns_null_for_an_unreported_row`.
 */
export function resolutionFor(
  coding: SubagentCoding | null | undefined,
  caller: ExecutionStyle | null,
): StyleResolution | null {
  const row: StyleResolutionRow | undefined = coding?.resolutions?.find(
    (r) => r.caller === caller,
  );
  return row?.resolution ?? null;
}

/**
 * Why: "did I get what I asked for?" is the one question the pane exists to
 * answer honestly, and it is NOT the same as "are there escalations" for the
 * no-override row — there, `requested` is whatever the config or built-in
 * supplied, and comparing against the user's (absent) choice would be
 * meaningless. Comparing `requested` against `effective` is the comparison
 * DOC-62 §3.4 actually specifies.
 * What: true when the resolved value differs from the one that entered
 * resolution.
 * Test: `wasOverruled_is_true_only_when_effective_differs_from_requested`.
 */
export function wasOverruled(resolution: StyleResolution): boolean {
  return resolution.requested !== null && resolution.requested !== resolution.effective;
}
