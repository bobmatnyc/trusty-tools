// Selected-assistant persistence (#4281 — persist the selected assistant
// across app launches; milestone M2, pillar (b) instance isolation).
//
// Why: "Assistant" is a TYPE and `izzie` / `cto-assistant` are INSTANCES of
// it, each with its own home, OKG and persona — so WHICH instance is selected
// is user-meaningful state, not an incidental UI toggle. It was nonetheless
// the only significant piece of client state that did not survive a reload:
// `activeAgentId` (`stores/app.ts`) was a plain in-memory
// `writable<string | null>(null)`, so every relaunch silently dropped the
// user back to Concierge, while the API token (`stores/app.ts`) and the theme
// (`stores/theme.ts`) already round-tripped through localStorage. Per the
// owner decision of 2026-07-28 the stickiness is PER-APP-LAUNCH (last-used,
// persisted client-side), NOT per-workstream — a per-workstream binding would
// cut against DOC-54 §9.2's "workstreams are filters, not containers", since
// filtering to a workstream would re-select a different assistant and
// reintroduce the context boundary DOC-54 rejects. Keeping the codec here as
// pure functions over an injectable `SelectionStorage` (rather than inline in
// the store) is what makes the failure modes — absent, corrupt, stale, or
// outright throwing storage — testable without a browser.
//
// What: the on-disk encoding of one assistant selection plus the three
// operations over it — `readSelectedAssistant`, `persistSelectedAssistant`,
// and `reconcileSelectedAssistant`. `stores/app.ts` wires them to
// `activeAgentId`; nothing else needs to call them (writing `activeAgentId`
// persists automatically).
//
// Test: `selectedAssistant.test.ts` (codec + degradation) and
// `stores/selectedAssistant.store.test.ts` (the store wiring).

import { CONCIERGE_AGENT_ID, type RosterEntry } from './roster';

/**
 * Why: the `activeAgentId` axis encodes Concierge as `null`, but localStorage
 * only stores strings — and `null` must be distinguishable from "the key was
 * never written". Reusing `CONCIERGE_AGENT_ID` ('ctrl') as the on-disk
 * sentinel keeps the persisted vocabulary identical to the one the config
 * surface already addresses agents by (`configAgentName` in `roster.ts`), so
 * a picker (#4404) can persist a card's id verbatim without a second mapping
 * table. Reading `'ctrl'` back as `null` is also a correctness guard: `ctrl`
 * does legitimately appear in `$agentRoster`, and dispatching it BY NAME
 * routes through the tools-OFF persona path (`handlers.rs::
 * resolve_agent_for_chat`), silently stripping Concierge's delegation
 * capability — see `components/ChatHeader.svelte`'s header comment.
 */
export const SELECTED_ASSISTANT_KEY = 'tagent.selectedAssistant';

/**
 * Why: agent ids are filesystem-addressable slugs — the Rust side enforces
 * `[a-zA-Z0-9_-]+` (`is_valid_agent_slug`) and `slugify` in `roster.ts`
 * produces exactly that charset. Anything else in storage was not written by
 * this app (hand-edited, truncated, or a foreign key collision) and must be
 * treated as absent rather than forwarded into a dispatch payload. The length
 * bound stops a corrupt multi-megabyte value from ever reaching the wire.
 */
const SELECTION_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

/** The slice of the `Storage` API this module needs; injectable for tests. */
export interface SelectionStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/**
 * Why: this module runs in three environments — the Tauri webview, a plain
 * browser tab, and jsdom/SSR where `localStorage` may be undefined or (Safari
 * private mode, hardened enterprise policies) throw `SecurityError` on mere
 * access. Resolving it through a guarded helper means every caller degrades to
 * "no persistence" instead of throwing during module initialization, which
 * would take the whole app down before first paint.
 * What: returns `globalThis.localStorage`, or `null` when it is unavailable.
 * Test: `defaultSelectionStorage_returns_null_when_unavailable`.
 */
export function defaultSelectionStorage(): SelectionStorage | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

/**
 * Why: startup must never block or panic on state the user can edit by hand.
 * A missing key (first launch), a corrupt value (hand-edited storage), or a
 * storage object that throws all mean the same thing to the app — no
 * remembered selection — and all resolve to Concierge, the pre-#4281 default.
 * Corrupt values are additionally EVICTED so the app self-heals rather than
 * re-parsing the same garbage on every launch.
 * What: reads the persisted selection and returns it in `activeAgentId`'s own
 * vocabulary — `null` for Concierge (absent, corrupt, or the explicit `ctrl`
 * sentinel), otherwise the assistant instance's id.
 * Test: `readSelectedAssistant_absent_key_is_concierge`,
 * `readSelectedAssistant_returns_a_persisted_instance_id`,
 * `readSelectedAssistant_ctrl_sentinel_is_concierge`,
 * `readSelectedAssistant_corrupt_value_is_concierge_and_evicted`,
 * `readSelectedAssistant_survives_throwing_storage`.
 */
export function readSelectedAssistant(
  storage: SelectionStorage | null = defaultSelectionStorage(),
): string | null {
  if (!storage) return null;
  let raw: string | null;
  try {
    raw = storage.getItem(SELECTED_ASSISTANT_KEY);
  } catch {
    return null;
  }
  if (raw === null) return null;
  const value = raw.trim();
  if (!SELECTION_PATTERN.test(value)) {
    // Self-heal: a value this app could not have written is never re-read.
    try {
      storage.removeItem(SELECTED_ASSISTANT_KEY);
    } catch {
      /* storage is read-only or unavailable — nothing further to do */
    }
    return null;
  }
  return value === CONCIERGE_AGENT_ID ? null : value;
}

/**
 * Why: the write half of the same contract — and the reason it is total
 * (Concierge included, via the sentinel) rather than "clear the key for
 * `null`": deliberately switching BACK to Concierge is a selection the user
 * expects to stick just as much as switching to Izzie, and clearing the key
 * would make it indistinguishable from a fresh install only by luck of the
 * default agreeing.
 * What: writes `id`, encoding Concierge (`null`) as the `ctrl` sentinel.
 * Silently no-ops when storage is unavailable or rejects the write (quota
 * exhausted, private mode) — persistence is a convenience, never a
 * precondition for using the app.
 * Test: `persistSelectedAssistant_round_trips_an_instance_id`,
 * `persistSelectedAssistant_encodes_concierge_as_the_ctrl_sentinel`,
 * `persistSelectedAssistant_survives_throwing_storage`.
 */
export function persistSelectedAssistant(
  id: string | null,
  storage: SelectionStorage | null = defaultSelectionStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(SELECTED_ASSISTANT_KEY, id ?? CONCIERGE_AGENT_ID);
  } catch {
    /* quota exceeded / private mode — the in-memory selection still works */
  }
}

/**
 * Why: a persisted selection is a POINTER into a roster the user can change
 * between launches — deleting `izzie`'s overlay, or opening the app against a
 * different project whose catalog never contained it. Left unchecked, that
 * stale pointer would ride into `TaskRequest.agent` and fail server-side on
 * the user's first message. The one case that must NOT be treated as stale is
 * an EMPTY roster: the catalog fetch races the cold-start sidecar (see
 * `App.svelte`'s `refetchPickerCatalogsOnReady`) and fails outright when the
 * API is unreachable, so "no entries" means "not loaded", never "your
 * assistant is gone". Erring that way costs a stale label until the roster
 * arrives; erring the other way would silently discard a valid selection on
 * every cold start — exactly the bug #4281 exists to fix.
 * What: returns `id` when it still names a roster entry (or when the roster is
 * empty/unloaded), and `null` — Concierge — when the roster is populated and
 * has no such entry. Pure; the caller decides when to apply it.
 * Test: `reconcileSelectedAssistant_keeps_a_live_selection`,
 * `reconcileSelectedAssistant_drops_a_stale_selection_to_concierge`,
 * `reconcileSelectedAssistant_keeps_selection_while_roster_is_empty`,
 * `reconcileSelectedAssistant_passes_concierge_through`.
 */
export function reconcileSelectedAssistant(
  id: string | null,
  roster: RosterEntry[],
): string | null {
  if (id === null) return null;
  if (roster.length === 0) return id;
  return roster.some((entry) => entry.id === id) ? id : null;
}
