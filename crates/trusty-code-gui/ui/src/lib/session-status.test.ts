// Why: `pickActiveSession` is the one piece of client-side logic in the
// status bar (which session to poll when the daemon has more than one) —
// worth pinning directly since a wrong pick silently shows the wrong
// session's readiness with no error.
// What: Three cases matching the function's documented contract.
// Test: this file.
import { describe, expect, it } from 'vitest';
import { pickActiveSession, type SessionSummary } from './session-status';

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
