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

import { writable } from 'svelte/store';

/** True while the agent-configuration surface has taken over the pane. */
export const configPaneOpen = writable(false);

export function openConfigPane(): void {
  configPaneOpen.set(true);
}

export function closeConfigPane(): void {
  configPaneOpen.set(false);
}

export function toggleConfigPane(): void {
  configPaneOpen.update((open) => !open);
}

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
