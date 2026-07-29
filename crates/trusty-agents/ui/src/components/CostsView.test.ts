// Costs pane rendering tests (#4098, COST-09).
//
// Why: the pane's value is that it tells four states apart. The one that
// matters most is NO LOG YET — a confident `$0.00` there is the failure this
// tab exists to avoid — so it gets an explicit assertion that no dollar figure
// is rendered at all. The rest pin that loading, empty-but-present, malformed
// and error each keep their own voice.
// What: Mounts `CostsView` with an injected `state` prop (standing in for the
// fetch) and asserts the rendered text.
// Test: this file.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import CostsView from './CostsView.svelte';
import type { CostRow, CostSummary, CostsState } from '../lib/costs';

let target: HTMLDivElement;
let instance: Record<string, unknown> | null = null;

function row(key: string, cost: number, calls = 1): CostRow {
  return {
    key,
    input_tokens: 1_000_000,
    output_tokens: 2_000,
    cost_usd: cost,
    dispatch_count: calls,
    duration_ms: 100,
  };
}

function summary(over: Partial<CostSummary> = {}): CostSummary {
  return {
    available: true,
    source: '/p/.trusty-agents/state/usage.jsonl',
    records: 2,
    malformed_lines: 0,
    first_ts: '2026-07-27T10:00:00Z',
    last_ts: '2026-07-28T10:00:00Z',
    window_days: null,
    totals: row('total', 3.8, 2),
    by_agent: [row('assistant', 3.0), row('ctrl', 0.8)],
    by_model: [row('anthropic/claude-sonnet-4-6', 3.0), row('anthropic/claude-haiku-4', 0.8)],
    by_date: [row('2026-07-27', 3.0), row('2026-07-28', 0.8)],
    ...over,
  };
}

function render(state: CostsState) {
  instance = mount(CostsView, { target, props: { state } }) as unknown as Record<string, unknown>;
}

/**
 * Rendered text with all runs of whitespace collapsed to single spaces.
 *
 * Why: Svelte emits the template's own newlines and indentation into
 * `textContent`, so a sentence that wraps across source lines — the malformed-
 * lines warning does — is NOT a contiguous substring of the raw text even
 * though it renders as one to a reader. Asserting on the raw value makes a
 * passing test depend on where the markup happens to wrap, which is a
 * formatting detail, not behaviour.
 * What: `textContent` with `\s+` collapsed and the ends trimmed.
 */
function text(): string {
  return (target.textContent ?? '').replace(/\s+/g, ' ').trim();
}

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  target.remove();
});

describe('CostsView — populated', () => {
  it('renders the grand total and the by-agent breakdown by default', () => {
    render({ kind: 'ok', summary: summary() });
    expect(text()).toContain('$3.80');
    expect(text()).toContain('2 dispatches');
    expect(text()).toContain('assistant');
    expect(text()).toContain('ctrl');
    // Not the model grouping until the control is switched.
    expect(text()).not.toContain('claude-sonnet-4-6');
  });

  it('draws bars scaled to the largest row, with no charting dependency', () => {
    render({ kind: 'ok', summary: summary() });
    const bars = target.querySelectorAll('div[style*="width"]');
    expect(bars.length).toBeGreaterThan(0);
    const widths = Array.from(bars).map((b) => (b as HTMLElement).style.width);
    expect(widths).toContain('100%');
  });

  it('labels the estimate as an estimate, not an invoice', () => {
    render({ kind: 'ok', summary: summary() });
    expect(text()).toContain('not a billed invoice');
  });
});

describe('CostsView — the four states', () => {
  it('shows a loading state rather than an empty chart', () => {
    render(null as unknown as CostsState);
    expect(text()).toContain('Loading cost data');
    expect(text()).not.toContain('$0.00');
  });

  it('says NO DATA for a missing log and never renders a dollar figure', () => {
    render({
      kind: 'no-data',
      reason: 'no usage has been recorded for this project yet',
      source: '/p/.trusty-agents/state/usage.jsonl',
    });
    expect(text()).toContain('No usage has been recorded');
    expect(text()).toContain('This is not $0.00');
    expect(text()).toContain('usage.jsonl');
    // The load-bearing assertion: no total, no table, no confident zero.
    expect(text()).not.toContain('dispatches');
    expect(target.querySelector('table')).toBeNull();
  });

  it('distinguishes an empty-but-present log from a missing one', () => {
    render({
      kind: 'ok',
      summary: summary({ records: 0, totals: row('total', 0, 0), by_agent: [], by_model: [], by_date: [] }),
    });
    expect(text()).toContain('No cost data for this period');
    expect(text()).toContain('usage log exists');
    expect(text()).not.toContain('No usage has been recorded');
  });

  it('surfaces the server error instead of an empty dataset', () => {
    render({ kind: 'error', message: 'could not read usage log /p/usage.jsonl: permission denied' });
    expect(text()).toContain('Could not read cost data');
    expect(text()).toContain('permission denied');
    expect(target.querySelector('table')).toBeNull();
  });
});

describe('CostsView — incomplete data', () => {
  it('warns that totals are incomplete when lines failed to parse', () => {
    render({ kind: 'ok', summary: summary({ malformed_lines: 3 }) });
    expect(text()).toContain('3 lines of the usage log could not be parsed');
    expect(text()).toContain('totals are incomplete');
    // Still renders what it could read.
    expect(text()).toContain('$3.80');
  });

  it('does not warn when every line parsed', () => {
    render({ kind: 'ok', summary: summary() });
    expect(text()).not.toContain('could not be parsed');
  });
});
