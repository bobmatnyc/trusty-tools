// Why: `pickActiveSession` is the one piece of client-side logic in the
// status bar (which session to poll when the daemon has more than one) —
// worth pinning directly since a wrong pick silently shows the wrong
// session's readiness with no error. `pickActiveSessionInWorkstream`
// (issue #3384) is the same rule scoped to a workstream's own bound
// sessions — `WorkstreamActivity.svelte` depends on it never picking a
// session bound to a DIFFERENT workstream.
// What: Cases matching each function's documented contract.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  pickActiveSession,
  pickActiveSessionInWorkstream,
  type SessionSummary,
} from './session-status';

function session(id: string, status: string, created_at: string): SessionSummary {
  return { id, status, created_at };
}

describe('pickActiveSession', () => {
  it('returns null for an empty list', () => {
    expect(pickActiveSession([])).toBeNull();
  });

  it('picks the most recently created non-terminal session', () => {
    const sessions = [
      session('older-running', 'running', '2026-07-18T10:00:00Z'),
      session('newer-running', 'running', '2026-07-18T12:00:00Z'),
      session('newest-finished', 'finished', '2026-07-18T13:00:00Z'),
    ];
    expect(pickActiveSession(sessions)?.id).toBe('newer-running');
  });

  it('falls back to the most recent overall when every session is terminal', () => {
    const sessions = [
      session('older-failed', 'failed', '2026-07-18T10:00:00Z'),
      session('newer-cancelled', 'cancelled', '2026-07-18T12:00:00Z'),
    ];
    expect(pickActiveSession(sessions)?.id).toBe('newer-cancelled');
  });
});

describe('pickActiveSessionInWorkstream', () => {
  it('returns null when the workstream has no bound sessions, without inspecting the sessions list', () => {
    const sessions = [session('unrelated', 'running', '2026-07-18T10:00:00Z')];
    expect(pickActiveSessionInWorkstream(sessions, [])).toBeNull();
  });

  it('never picks a session bound to a DIFFERENT workstream', () => {
    const sessions = [
      session('other-workstreams-session', 'running', '2026-07-18T12:00:00Z'),
      session('this-workstreams-session', 'running', '2026-07-18T10:00:00Z'),
    ];
    expect(pickActiveSessionInWorkstream(sessions, ['this-workstreams-session'])?.id).toBe(
      'this-workstreams-session',
    );
  });

  it('applies the same recency/terminal-status heuristic within the filtered set', () => {
    const sessions = [
      session('bound-older-running', 'running', '2026-07-18T10:00:00Z'),
      session('bound-newer-finished', 'finished', '2026-07-18T13:00:00Z'),
      session('unbound-newest', 'running', '2026-07-18T14:00:00Z'),
    ];
    expect(
      pickActiveSessionInWorkstream(sessions, ['bound-older-running', 'bound-newer-finished'])?.id,
    ).toBe('bound-older-running');
  });

  it('returns null when the workstream has bound session ids but none match a listed session', () => {
    const sessions = [session('some-other-session', 'running', '2026-07-18T10:00:00Z')];
    expect(pickActiveSessionInWorkstream(sessions, ['vanished-session'])).toBeNull();
  });
});
