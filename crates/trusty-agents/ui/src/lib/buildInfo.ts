// Running-daemon build provenance for the app header (owner ask: "we need to
// show version in the header").
//
// Why: the header answers "what app is this, what am I looking at, is it
// connected" — but not "WHICH BUILD am I actually talking to". That gap bit us
// on 2026-07-28: the running app was ~3.5h stale and missing a merged fix, and
// diagnosing it needed a filesystem mtime comparison plus a `strings` dump of
// the binary. The version must therefore come from the DAEMON
// (`GET /api/health`), not from a compile-time constant baked into the
// frontend — the whole point is to surface a UI/daemon mismatch, which a
// self-reported constant cannot do.
//
// A version string alone is only half the fix: both the stale build and the
// current build reported `0.38.6`. Issue #4260 adds real provenance (git SHA,
// dirty flag, build timestamp) to `tagent --version` and `/api/health`; this
// module is shaped so that lands as a data change, not a redesign — see
// `parseHealthBody` and `formatProvenance`.
//
// NOTE: this deliberately does NOT surface the `build #N` counter from
// `tagent --version`. That counter increments on every invocation and says
// nothing about which source built the binary — displaying it is exactly the
// false-confidence #4260 exists to remove.
//
// Test: `buildInfo.test.ts`.

import { apiBase } from './api-config';

/**
 * Why: the header must never flash a wrong value and must never render an
 * empty slot that shifts layout, so "we haven't asked yet" and "we asked and
 * got nothing" are distinct, explicitly-rendered states rather than a falsy
 * version string.
 * What: `loading` before `/api/health` has answered, `ready` with the daemon's
 * self-reported version (plus, once #4260 lands, its short commit), and
 * `unavailable` when the probe failed or returned an unusable body.
 * Test: `buildInfo.test.ts` — every variant round-trips through
 * `formatProvenance`.
 */
export type BuildInfoState =
  | { status: 'loading' }
  | { status: 'ready'; version: string; commit?: string }
  | { status: 'unavailable' };

/**
 * Why: `/api/health` is an unauthenticated liveness probe whose body we do not
 * control across daemon versions — an older daemon omits fields a newer UI
 * expects, and (post-#4260) a newer daemon carries fields this UI predates.
 * Parsing defensively in one pure function keeps that tolerance testable
 * without a network round-trip.
 * What: Accepts the decoded JSON body and narrows it to a `BuildInfoState`. A
 * non-empty string `version` is required; anything else is `unavailable`.
 * `commit` is the #4260 SLOT — the daemon does not send it today, so it parses
 * as `undefined` and the header simply renders the version alone. When #4260
 * ships, confirm the field name it actually emits (this assumes `commit`
 * carrying a short SHA; #4260 also plans a dirty flag and build timestamp,
 * which would extend `BuildInfoState` and `formatProvenance` here) and the
 * header picks it up with no component change.
 * Test: `buildInfo.test.ts` — missing/blank/non-string version → unavailable;
 * version-only → ready without commit; version+commit → ready with both.
 */
export function parseHealthBody(raw: unknown): BuildInfoState {
  if (typeof raw !== 'object' || raw === null) return { status: 'unavailable' };
  const body = raw as { version?: unknown; commit?: unknown };
  const version = typeof body.version === 'string' ? body.version.trim() : '';
  if (!version) return { status: 'unavailable' };
  const rawCommit = typeof body.commit === 'string' ? body.commit.trim() : '';
  return rawCommit ? { status: 'ready', version, commit: rawCommit } : { status: 'ready', version };
}

/**
 * Why: the header renders ONE provenance line whose width is stable across
 * states, so the reader always knows whether the app is still asking, has an
 * answer, or gave up — and so the future SHA appends to the same line instead
 * of needing new chrome. Keeping the string formatting pure means the
 * loading/failure copy is pinned by tests rather than by reading the markup.
 * What: `v…` while loading (honest "not known yet", never a stale or guessed
 * number), `v—` when the probe failed, and `v<version>` once known — gaining
 * ` · <short-sha>` automatically when #4260 starts sending one.
 * Test: `buildInfo.test.ts`.
 */
export function formatProvenance(state: BuildInfoState): string {
  switch (state.status) {
    case 'ready':
      return state.commit ? `v${state.version} · ${state.commit}` : `v${state.version}`;
    case 'unavailable':
      return 'v—';
    default:
      return 'v…';
  }
}

/**
 * Why: a human hovering the line needs to know whether a missing value means
 * "still asking" or "this daemon can't tell me", and — once a version IS shown
 * — that it is the DAEMON's version, not the frontend's. The tooltip carries
 * that provenance so the visible line can stay a single short token.
 * What: The `title` text for the provenance element, per state.
 * Test: `buildInfo.test.ts`.
 */
export function provenanceTitle(state: BuildInfoState): string {
  switch (state.status) {
    case 'ready':
      return `Running daemon version ${state.version} (reported by GET /api/health)`;
    case 'unavailable':
      return 'Daemon version unavailable — GET /api/health did not report one';
    default:
      return 'Reading the running daemon version from GET /api/health…';
  }
}

/**
 * Why: `invoke('check_health')` already exists on both transports but returns
 * a bare `boolean` (Tauri: `task_commands::check_health`; browser:
 * `transport.ts`'s `fetchFallback`), so it cannot carry the version — and
 * widening its contract would ripple through `App.svelte`'s 40-attempt
 * bootstrap loop for no benefit. This is instead a ONE-SHOT read fired after
 * that existing readiness probe has already succeeded: no second poller, no
 * retry loop, one request per app lifetime.
 * What: GETs `<apiBase>/api/health` (unauthenticated — see `transport.ts`'s
 * `authHeaders` note) and narrows the body via `parseHealthBody`. Never
 * rejects: any transport, status, or decode failure resolves to
 * `unavailable`, because a header ornament must not surface an unhandled
 * rejection.
 * Test: `buildInfo.test.ts` — injects a stub `fetchImpl` for the ok / non-ok /
 * throwing / malformed-JSON cases.
 */
export async function fetchBuildInfo(fetchImpl: typeof fetch = fetch): Promise<BuildInfoState> {
  try {
    const r = await fetchImpl(`${apiBase()}/api/health`);
    if (!r.ok) return { status: 'unavailable' };
    return parseHealthBody(await r.json());
  } catch {
    return { status: 'unavailable' };
  }
}
