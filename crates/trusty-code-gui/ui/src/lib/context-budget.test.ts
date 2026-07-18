// Why: `classifyBudget`/`budgetLabel`/`budgetDotClass`/`budgetTitle` are the
// status bar's only client-side logic for the budget slot — a flipped
// threshold or a fabricated "0%" for `never_recorded` would be silently
// wrong with no error to surface it, so each case is pinned directly.
// What: covers the three query shapes (`null`, `never_recorded`, `recorded`)
// crossed with the `within_budget`/`compaction_fired` flags that change
// output.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  budgetDotClass,
  budgetLabel,
  budgetTitle,
  classifyBudget,
  type ContextBudgetQuery,
} from './context-budget';

function recorded(overrides: Partial<Extract<ContextBudgetQuery, { status: 'recorded' }>> = {}) {
  return {
    status: 'recorded' as const,
    context_window_tokens: 200_000,
    overhead_tokens: 50_000,
    overhead_cap_tokens: 80_000,
    working_context_pct: 75,
    overhead_pct: 25,
    within_budget: true,
    compaction_fired: false,
    compaction_rounds: 0,
    ...overrides,
  };
}

const neverRecorded: ContextBudgetQuery = { status: 'never_recorded' };

describe('classifyBudget', () => {
  it('treats null (not fetched yet) as no-data', () => {
    expect(classifyBudget(null)).toBe('no-data');
  });

  it('treats never_recorded as no-data', () => {
    expect(classifyBudget(neverRecorded)).toBe('no-data');
  });

  it('classifies within_budget=true as ok', () => {
    expect(classifyBudget(recorded({ within_budget: true }))).toBe('ok');
  });

  it('classifies within_budget=false as warn', () => {
    expect(classifyBudget(recorded({ within_budget: false }))).toBe('warn');
  });
});

describe('budgetDotClass', () => {
  it('maps ok to bg-status-ok', () => {
    expect(budgetDotClass(recorded({ within_budget: true }))).toBe('bg-status-ok');
  });

  it('maps warn to bg-status-error (DOC-39 §4.5 AC-5.7 red gap token)', () => {
    expect(budgetDotClass(recorded({ within_budget: false }))).toBe('bg-status-error');
  });

  it('maps no-data to bg-status-neutral', () => {
    expect(budgetDotClass(neverRecorded)).toBe('bg-status-neutral');
    expect(budgetDotClass(null)).toBe('bg-status-neutral');
  });
});

describe('budgetLabel', () => {
  it('renders "no data yet" for null, never a fake percentage', () => {
    expect(budgetLabel(null)).toBe('no data yet');
  });

  it('renders "no data yet" for never_recorded', () => {
    expect(budgetLabel(neverRecorded)).toBe('no data yet');
  });

  it('renders the working-context percentage for a recorded snapshot', () => {
    expect(budgetLabel(recorded({ working_context_pct: 82, compaction_fired: false }))).toBe(
      '82% working',
    );
  });

  it('appends a compaction cue when compaction_fired (AC-5.8)', () => {
    expect(budgetLabel(recorded({ working_context_pct: 58, compaction_fired: true }))).toBe(
      '58% working, compacted',
    );
  });
});

describe('budgetTitle', () => {
  it('explains the gap for null/never_recorded', () => {
    expect(budgetTitle(null)).toMatch(/no context-budget measurement/i);
    expect(budgetTitle(neverRecorded)).toMatch(/no context-budget measurement/i);
  });

  it('includes token counts for a recorded snapshot', () => {
    const title = budgetTitle(
      recorded({ context_window_tokens: 200_000, overhead_pct: 25, overhead_cap_tokens: 80_000 }),
    );
    expect(title).toContain('200000 tokens');
    expect(title).toContain('25%');
  });

  it('includes the compaction round count when compaction_fired', () => {
    const title = budgetTitle(recorded({ compaction_fired: true, compaction_rounds: 2 }));
    expect(title).toContain('compacted 2x');
  });
});
