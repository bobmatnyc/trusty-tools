// Agent-configuration takeover state (#3894, epic #3052).
//
// Why: Bob's directive — "when we're in agent configuration, let's take over
// that pane, we don't need chat when we're configuring". The gear button that
// opens the config surface lives in `ChatHeader`, INSIDE the chat column,
// but the surface it opens must cover the whole content area (chat column +
// recap rail). A component can't reach outside its own subtree, so the
// open/closed bit lives here, in a module-level store: `ChatHeader` writes it,
// `ChatPane` reads it to render `AgentConfigOverlay` as a SIBLING of the chat
// column (and to `inert` that column while the takeover is up).
// What: A boolean store plus the three mutations the UI needs and one pure
// key predicate. Deliberately holds NO agent identity — which agent is being
// configured is already `stores/app.ts`'s `activeAgentId`, and duplicating it
// here would let the two drift (a switch in the picker must retarget an open
// config pane, which falls out for free when there is only one source).
// Test: `configPane.test.ts`.

import { get, writable } from 'svelte/store';

/** True while the agent-configuration surface has taken over the pane. */
export const configPaneOpen = writable(false);

/**
 * Why (PR #3895 code-critic HIGH-1): Esc is habitual while editing text, and
 * the config surface holds long, hand-written system prompts. Every exit path
 * — Esc, Back, Close, and leaving the Chat tab — must therefore consult
 * whether the open panel has unsaved Personality/Tools edits before it
 * unmounts, or the takeover becomes a fresh data-loss path. The dirty bit is
 * published here (rather than kept private to `AgentConfigPanel`) because two
 * of those exit paths originate OUTSIDE the panel and need the answer
 * synchronously.
 * What: Mirrors `personalityDirty || toolsDirty`. Written by the panel while
 * it is mounted, cleared on its destroy.
 * Test: `ChatPane.test.ts` — every exit path with a dirty editor.
 */
export const configPaneDirty = writable(false);

/**
 * Why: a dirty exit has to be resolved by the panel (only it can save, and
 * only it knows which section is dirty), but the request can come from
 * anywhere. A monotonic nonce is the smallest thing that carries "someone
 * asked to leave" to a component without the store needing a handle on it.
 * What: Incremented by `requestExitConfigPane` when the pane is dirty; the
 * panel watches for a change and raises its confirm-before-discard prompt.
 */
export const configExitIntent = writable(0);

/**
 * Why: the single entry point every exit path funnels through, so the
 * dirty-check cannot be forgotten at one call site (it already was at three).
 * What: Closes the pane immediately when there is nothing to lose and returns
 * `true`. When the panel holds unsaved edits it instead raises the panel's
 * confirm prompt and returns `false`, letting a caller that was going
 * somewhere else (the Chat→Events tab switch) stay put.
 * Test: `configPane.test.ts` (both branches) and `ChatPane.test.ts` (each
 * exit affordance end to end).
 */
export function requestExitConfigPane(): boolean {
  if (!get(configPaneOpen)) return true;
  if (get(configPaneDirty)) {
    configExitIntent.update((n) => n + 1);
    return false;
  }
  closeConfigPane();
  return true;
}

export function openConfigPane(): void {
  configPaneOpen.set(true);
}

export function closeConfigPane(): void {
  configPaneOpen.set(false);
}

// NB (PR #3895 re-review): there is deliberately NO `toggleConfigPane()`. It
// existed, the gear button bound to it, and it flipped `configPaneOpen` false
// without consulting `configPaneDirty` — a silent-discard bypass around the
// guard below. Leaving the pane goes through `requestExitConfigPane()`, always;
// `ChatHeader.handleGear` shows the open-vs-leave split a toggle would hide.

/**
 * Why: Esc must exit the takeover (Bob: "provide an obvious exit
 * affordance"), but a bare `event.key === 'Escape'` in the keydown handler
 * would also swallow the Esc of a chorded shortcut (Cmd+Esc opens the macOS
 * Force-Quit panel, Ctrl+Esc the Windows Start menu) — the OS handles those
 * and the app should not additionally act on them. Keeping the decision in a
 * pure predicate keeps the component handler a one-liner and makes the rule
 * unit-testable without mounting anything.
 * What: True only for an unmodified Escape press. Shift is ignored (Shift+Esc
 * carries no platform meaning and users routinely leave Shift down).
 * Test: `isConfigExitKey_true_for_plain_escape`,
 * `isConfigExitKey_false_for_other_keys`,
 * `isConfigExitKey_false_when_chorded_with_a_platform_modifier`.
 */
export function isConfigExitKey(
  event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'metaKey' | 'altKey'>,
): boolean {
  if (event.key !== 'Escape') return false;
  return !event.ctrlKey && !event.metaKey && !event.altKey;
}
