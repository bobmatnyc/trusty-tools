import { writable } from 'svelte/store';

/**
 * Why: Session recaps (#371) arrive over SSE as `recap_generated` events and
 * need to surface in two places — the chat thread (as a special "recap" role
 * message) and the per-session RecapPanel between ChatView and InputArea.
 * Both consumers need the latest recap keyed by session_id, so we hold a Map
 * and update it once per incoming event.
 * What: A writable Map<session_id, Recap>. `setRecap` replaces the entry for a
 * given session and triggers reactivity by re-emitting the Map.
 * Test: Call `setRecap({session_id:'s1', summary:'x', table_rows:[], received_at:Date.now()})`,
 * subscribe to `recaps`, assert `.get('s1').summary === 'x'`.
 */
export interface Recap {
  session_id: string;
  summary: string;
  table_rows: [string, string][];
  received_at: number;
}

export const recaps = writable<Map<string, Recap>>(new Map());

export function setRecap(r: Recap): void {
  recaps.update((m) => {
    const next = new Map(m);
    next.set(r.session_id, r);
    return next;
  });
}
