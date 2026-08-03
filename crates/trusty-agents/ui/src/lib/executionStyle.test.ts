// Why (#4353, DOC-62): these helpers exist to keep two spec obligations from
// being lost in markup. OQ-3's mitigation for the `engineer` style /
// `engineer` sub-agent collision is PRESENTATIONAL, and the spec says plainly
// that makes it "easy to lose" — so the label is asserted here rather than
// trusted to a template. SM-9's `vibe` → `engineer` fail-safe must read as the
// specified behaviour, not as a bug, so its sentence is asserted too.
// The lookup helpers are pinned for the property that matters at the call site:
// rows are matched by `caller`, never by position, because the server owns the
// row order and a positional read would mis-attribute a resolution silently.
// What: pure unit tests. Nothing here resolves a style — that is the server's
// job and is pinned in `agent_subagents.rs`'s Rust tests.
// Test: this file.
import { describe, expect, it } from 'vitest';
import type { StyleResolution, SubagentCoding } from './agentConfig';
import {
  STYLE_PRESENTATION,
  escalationSentence,
  resolutionFor,
  sourceSentence,
  styleLabel,
  wasOverruled,
} from './executionStyle';

function resolution(over: Partial<StyleResolution> = {}): StyleResolution {
  return {
    requested: 'vibe',
    source: 'caller',
    effective: 'engineer',
    escalations: ['tier-unimplemented'],
    ...over,
  };
}

function coding(over: Partial<SubagentCoding> = {}): SubagentCoding {
  return {
    mechanism: 'coding',
    tool: 'dispatch_task',
    tool_granted: true,
    target: 'coding-pm',
    gated_by_allowed: false,
    lane_floor: 'vibe',
    built_in_default: 'engineer',
    config_default: null,
    resolutions: [
      {
        caller: null,
        resolution: resolution({ requested: null, source: 'built-in', escalations: [] }),
      },
      {
        caller: 'hack',
        resolution: resolution({
          requested: 'hack',
          escalations: ['callee-floor', 'tier-unimplemented'],
        }),
      },
      { caller: 'vibe', resolution: resolution() },
      {
        caller: 'engineer',
        resolution: resolution({ requested: 'engineer', escalations: [] }),
      },
    ],
    ...over,
  };
}

describe('style labels (DOC-62 OQ-3)', () => {
  // The collision the spec flags: a user reading `style = engineer` as
  // "delegate to the engineer agent" has read it as something epic #4345
  // decision 2 forbids. The recommended mitigation is the GUI label, so the
  // label must never be the bare wire value.
  it('disambiguates the engineer STYLE from the engineer sub-agent role', () => {
    expect(styleLabel('engineer')).not.toBe('engineer');
    expect(styleLabel('engineer').toLowerCase()).toContain('full loop');
    // …and the meaning states the distinction outright, since a label alone
    // cannot carry it.
    const meaning = STYLE_PRESENTATION.engineer.meaning;
    expect(meaning).toContain('CEREMONY LEVEL');
    expect(meaning).toContain('sub-agent');
  });

  it('presents every style with a label and a meaning', () => {
    for (const style of ['hack', 'vibe', 'engineer'] as const) {
      expect(STYLE_PRESENTATION[style].label).toBeTruthy();
      expect(STYLE_PRESENTATION[style].meaning).toBeTruthy();
      // A label that were just the wire value would defeat the whole table.
      expect(STYLE_PRESENTATION[style].label).not.toBe(style);
    }
  });

  it('names vibe as unimplemented so the tier is not oversold', () => {
    expect(STYLE_PRESENTATION.vibe.meaning).toContain('#2596');
  });
});

describe('resolution path sentences (DOC-62 §3.4)', () => {
  it('names each precedence level distinctly', () => {
    const sentences = (['caller', 'config', 'built-in'] as const).map(sourceSentence);
    expect(new Set(sentences).size).toBe(3);
    expect(sourceSentence('config')).toContain('default_style');
    expect(sourceSentence('built-in')).toContain('built-in');
  });

  it('explains both specified escalation reasons', () => {
    expect(escalationSentence('callee-floor')).toContain('floor');
    // SM-9 must read as deliberate, not as a defect — the sentence has to say
    // the higher tier RAN and was reported, and cite the open issue.
    const sm9 = escalationSentence('tier-unimplemented');
    expect(sm9).toContain('no implementation yet');
    expect(sm9).toContain('#2596');
  });

  it('degrades on an unknown reason instead of dropping it', () => {
    // A newer sidecar could report a reason this build does not know. An
    // unexplained raise is still a raise the user is entitled to see.
    const sentence = escalationSentence('some-future-reason');
    expect(sentence).toContain('some-future-reason');
    expect(sentence).toContain('Raised');
  });
});

describe('resolutionFor', () => {
  it('matches by caller, not by position', () => {
    // Deliberately reversed order: a positional lookup would return the wrong
    // row and the pane would report another style's resolution as this one's.
    const reversed = coding({ resolutions: [...coding().resolutions].reverse() });
    expect(resolutionFor(reversed, 'hack')?.requested).toBe('hack');
    expect(resolutionFor(reversed, null)?.requested).toBeNull();
  });

  it('returns null for a row the server did not report', () => {
    expect(resolutionFor(coding({ resolutions: [] }), 'vibe')).toBeNull();
  });

  it('returns null when the lane itself is unreported', () => {
    // A pre-#4353 sidecar. The pane must say so, never fabricate a resolution.
    expect(resolutionFor(undefined, null)).toBeNull();
    expect(resolutionFor(null, 'engineer')).toBeNull();
  });
});

describe('wasOverruled', () => {
  it('is true exactly when the effective style differs from the request', () => {
    expect(wasOverruled(resolution({ requested: 'vibe', effective: 'engineer' }))).toBe(true);
    expect(wasOverruled(resolution({ requested: 'engineer', effective: 'engineer' }))).toBe(
      false,
    );
  });

  it('is false when nothing was requested at any level', () => {
    // The no-override row with no config default: `requested` is null and the
    // built-in applied. Calling that "overruled" would flag a normal default
    // as an override on every unconfigured agent.
    expect(wasOverruled(resolution({ requested: null, source: 'built-in' }))).toBe(false);
  });
});
