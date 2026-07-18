// Why: `StatusBar.svelte` needs the wire shapes for `GET /sessions` and
// `GET /sessions/{id}/readiness` (REST Slice 2, issue #2983) plus the small
// "which session does the status bar reflect" selection rule. Extracted from
// the component so the selection logic (a pure function) is testable and
// reviewable independent of DOM/polling concerns — mirrors the split already
// established between `api-config.ts` (transport) and `HealthPanel.svelte`
// (rendering).
// What: Wire types matching `crates/trusty-code/src/session/model.rs`
// (`Session`) and `crates/trusty-code/src/session/registry_events.rs`
// (`ReadinessQuery`), plus `pickActiveSession` — the client-side "most
// recent session" heuristic. This is a display-ordering choice only (no
// session picker/workstream switcher exists yet — that is Phase 2+ per
// DOC-39 §6.3); it computes nothing the daemon doesn't already own.
// Test: `session-status.test.ts`.

/** Mirrors `crate::session::model::Session` (id/status/created_at only — the
 * fields the status bar actually reads). */
export interface SessionSummary {
  id: string;
  status: string;
  created_at: string;
  project?: string | null;
}

/** `GET /sessions` response envelope. */
export interface SessionListResponse {
  sessions: SessionSummary[];
}

/** Mirrors `crate::session::registry_events::ReadinessQuery`
 * (`#[serde(tag = "status")]`). */
export type ReadinessQuery =
  | {
      status: 'probed';
      state: 'ready' | 'warming' | 'unavailable';
      index_id: string | null;
      lifecycle_status: string | null;
      chunk_count: number | null;
      lexical_ready: boolean;
      semantic_ready: boolean;
      graph_ready: boolean;
      summary: string;
    }
  | { status: 'never_probed' };

/**
 * Pick the session the status bar reflects when the daemon has more than
 * one: the most recently created **non-terminal** session, so the bar
 * tracks the workstream the operator is actively looking at rather than a
 * finished/cancelled one; falls back to the most recent overall if every
 * session is terminal.
 *
 * Why: Phase 1 has no session picker/workstream switcher (DOC-39 §6.3
 * defers that) — the status bar still needs *a* session to poll. This is a
 * display-ordering heuristic over data the daemon already returns, not new
 * business logic.
 * What: Returns `null` for an empty list. Sorts by `created_at` descending
 * (ISO 8601 strings sort correctly lexically).
 * Test: `session-status.test.ts::picks-most-recent-non-terminal`,
 * `::falls-back-to-most-recent-when-all-terminal`,
 * `::returns-null-for-empty-list`.
 */
export function pickActiveSession(sessions: SessionSummary[]): SessionSummary | null {
  if (sessions.length === 0) return null;
  const terminal = new Set(['cancelled', 'finished', 'failed', 'deadline_exceeded']);
  const byRecency = [...sessions].sort((a, b) => b.created_at.localeCompare(a.created_at));
  return byRecency.find((s) => !terminal.has(s.status)) ?? byRecency[0];
}
