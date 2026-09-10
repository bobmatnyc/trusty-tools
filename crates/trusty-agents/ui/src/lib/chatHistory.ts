// Chat rehydration from the durable persona log (#4278).
//
// Why: `messages` (`stores/app.ts`) starts as an empty Map on every load and
// is only ever appended to by `InputArea`, `eventBridge`, and `Sidebar`.
// Nothing read the persisted log back, so a plain page reload discarded the
// whole visible conversation while the durable record sat untouched in
// trusty-memory — a data-loss bug, not a missing feature. The two surfaces
// that look like they could serve cannot: `GET /api/tasks` returns
// `PmResponse` envelopes carrying NO request text (see `taskHistory.ts`), so
// it can never restore a prompt; and `GET /api/workstreams/:name/history` is
// the 10-item digest `resumeWorkstream` already injects as a banner, which the
// owner ruled does not satisfy this issue. `GET /api/agents/:name/chat-history`
// reads the `chat_session` projection keyed `persona-{agent}` — the only store
// holding both halves of a turn.
//
// What: `fetchChatHistory` — the thin REST wrapper, returning an unavailable
// envelope rather than throwing so a down daemon can never break the chat.
// `historyToMessages` — the pure, unit-tested mapper from the wire's flat
// `{role, content}` list to the `Message` shape `ChatView` renders.
//
// Bounded by construction: the `persona-{agent}` session is continuous and
// never rolls over, so the initial load asks for `DEFAULT_HISTORY_LIMIT`
// messages and `has_more` + the `before` cursor drive lazy loading of older
// turns on demand.
//
// Test: `chatHistory.test.ts`.

import { get } from 'svelte/store';

import { apiBase } from './api-config';
import {
  canLoadOlderChat,
  conversationKey,
  activeConversationKey,
  messages,
  isRunning,
  type ChatHistoryCursor,
  chatHistoryCursor,
  getCurrentApiToken,
  hydrateMessages,
  loadingOlderChat,
  prependMessages,
  type Message,
} from '../stores/app';

/** One persisted message, as `trusty_common`'s `ChatMessage` serializes. */
export interface ChatHistoryMessage {
  role: string;
  content: string;
}

/** `GET /api/agents/:name/chat-history` response body. */
export interface ChatHistoryPage {
  /**
   * False whenever there is nothing to rehydrate — the agent binds no memory
   * palace, its session does not exist yet, or trusty-memory is unreachable.
   * The route answers 200 in all three cases, so the chat view renders an
   * empty conversation rather than an error.
   */
  available: boolean;
  session_absent?: boolean;
  messages: ChatHistoryMessage[];
  /**
   * ABSOLUTE index of this page's first message. Passed straight back as the
   * next `until` to page backwards — see `fetchChatHistory`.
   */
  start: number;
  /** Total messages in the session, not in this page. */
  total: number;
  /** Whether older messages exist before this page's oldest. */
  has_more: boolean;
  /** Session-level timestamp; the store keeps no per-message time. */
  updated_at: string | null;
  reason?: string;
}

/**
 * Initial page size, in MESSAGES (a turn is a user/assistant pair, so this is
 * roughly 50 turns). Named in messages because that is what the store holds.
 */
export const DEFAULT_HISTORY_LIMIT = 100;

/**
 * The shape every failure path returns, so callers branch only on `available`.
 * Built per call rather than shared, so a `reason` can be attached without
 * mutating a module-level object every later caller would then see.
 */
function emptyPage(reason: string): ChatHistoryPage {
  return {
    available: false,
    messages: [],
    start: 0,
    total: 0,
    has_more: false,
    updated_at: null,
    reason,
  };
}

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * Why: rehydration runs on app start, when the sidecar may still be binding
 * its socket. A throw here would surface as an unhandled rejection during
 * bootstrap and leave the chat blank anyway, so every failure degrades to the
 * same empty page the backend returns for "nothing persisted yet".
 * What: `GET /api/agents/:name/chat-history?limit=&until=`. `until` is an
 * ABSOLUTE exclusive end index, omitted for the newest page. Absolute indexing
 * is what makes paging backwards stable while new turns append — an
 * offset-from-the-end cursor shifts by the number of appends and replays them.
 * Test: `fetchChatHistory_returns_empty_page_on_http_error`,
 * `fetchChatHistory_returns_empty_page_on_network_failure`,
 * `fetchChatHistory_requests_the_bounded_window`,
 * `fetchChatHistory_sends_until_only_when_paging_back`.
 */
export async function fetchChatHistory(
  agent: string,
  limit = DEFAULT_HISTORY_LIMIT,
  until?: number,
): Promise<ChatHistoryPage> {
  const cursor = until === undefined ? '' : `&until=${until}`;
  try {
    const r = await fetch(
      `${apiBase()}/api/agents/${encodeURIComponent(agent)}/chat-history` +
        `?limit=${limit}${cursor}`,
      { headers: authHeaders() },
    );
    if (!r.ok) return emptyPage(`chat history request failed (HTTP ${r.status})`);
    const body = (await r.json()) as Partial<ChatHistoryPage>;
    return {
      available: body.available === true,
      session_absent: body.session_absent === true,
      messages: Array.isArray(body.messages) ? body.messages : [],
      start: typeof body.start === 'number' ? body.start : 0,
      total: typeof body.total === 'number' ? body.total : 0,
      has_more: body.has_more === true,
      updated_at: typeof body.updated_at === 'string' ? body.updated_at : null,
      reason: body.reason,
    };
  } catch (e) {
    return emptyPage(`chat history unreachable: ${e}`);
  }
}

/**
 * Roles the chat store renders. `chat_turn_append` only ever writes `user` and
 * `assistant`, but the store's history blob is untyped JSON, so an unknown
 * role is mapped to `system` rather than cast blindly into a union it may not
 * belong to.
 */
function toRole(role: string): Message['role'] {
  return role === 'user' || role === 'assistant' ? role : 'system';
}

/**
 * Why: `ChatView` renders `Message`, not the wire shape, and it stamps a
 * timestamp on every bubble. The persisted store carries NO per-message
 * timestamp — `ChatMessage` is `{role, content}` — so a per-turn time would
 * have to be invented. It is not: every rehydrated bubble takes the session's
 * `updated_at`, the only real time the backend has, and the caller passes
 * `now` only as the fallback for a session that reports none.
 * What: pure map from the wire list to `Message[]`, preserving order.
 * `speaker` is stamped on assistant bubbles so a rehydrated conversation is
 * attributed the same way a live one is.
 * Test: `historyToMessages_maps_roles_and_preserves_order`,
 * `historyToMessages_stamps_session_time_not_a_fabricated_one`,
 * `historyToMessages_labels_unknown_roles_as_system`.
 */
export function historyToMessages(
  page: ChatHistoryPage,
  speaker: string,
  now: number,
): Message[] {
  const ts = page.updated_at ? Date.parse(page.updated_at) : NaN;
  const timestamp = Number.isNaN(ts) ? now : ts;
  return page.messages.map((m, i) => {
    let role = toRole(m.role);
    let content = m.content ?? '';
    let activity: Partial<Message> = {};
    if (m.role === 'system') {
      try {
        const event = JSON.parse(content);
        if (event?.kind === 'trusty.listener-event' && event.version === 1 && typeof event.listener === 'string' && typeof event.event_type === 'string') {
          role = 'event';
          if (typeof event.event_id === 'string') activity.eventId=event.event_id;
          content = [event.listener, event.event_type, typeof event.from === 'string' ? event.from : '', typeof event.subject === 'string' ? event.subject : ''].filter(Boolean).join(' · ');
        } else if (event?.kind === 'trusty.tool-activity' && event.version === 1 && typeof event.tool === 'string' && typeof event.call_id === 'string' && ['running','complete','error'].includes(event.status)) {
          role='tool'; content=''; activity={toolName:event.tool.slice(0,160),toolCallId:event.call_id,activityStatus:event.status === 'running' ? 'error' : event.status};
        }
      } catch { /* Ordinary system content stays an ordinary banner. */ }
    }
    return {
      // Stable and collision-free against live ids, which are uuid/task-based.
      id: `history-${page.start + i}`,
      role,
      content,
      timestamp,
      ...activity,
      ...(role === 'assistant' ? { speaker } : {}),
    };
  });
}

/**
 * Why: the persona log is keyed by AGENT (`persona-{agent}`) while the chat
 * store is keyed by project, so rehydration has to answer "whose history, and
 * under whose name" before it can fetch. `activeAgentId` is `null` for the
 * base ctrl/PM session (see its doc comment in `stores/app.ts`), and the
 * roster can still be empty on a cold start when the catalog fetch has not
 * landed — both have to resolve to something rather than fetching
 * `persona-null` or stamping `undefined` on every restored bubble.
 * What: pure resolution of `{agentId, speaker}` from the selection and the
 * roster. Extracted so the mapping is unit-testable, leaving `App.svelte` with
 * a one-line call rather than untested glue.
 * Test: `resolveRehydrationTarget_*`.
 */
export function resolveRehydrationTarget(
  activeAgentId: string | null,
  roster: { id: string; label: string }[],
): { agentId: string; speaker: string } {
  const agentId = activeAgentId ?? 'ctrl';
  const speaker = roster.find((entry) => entry.id === agentId)?.label ?? 'Assistant';
  return { agentId, speaker };
}

export interface RehydrateResult {
  /** Messages seeded into the store. 0 when nothing was persisted. */
  seeded: number;
  /** Whether older messages remain, for the "load earlier" affordance. */
  hasMore: boolean;
  /**
   * Why the load produced nothing, when it produced nothing. Every
   * `available: false` path — no palace bound, no session yet, daemon down,
   * malformed payload — arrives here, and all four render as an identically
   * empty chat. Without this the caller cannot tell a first-run empty
   * conversation from a broken one, so a real failure is invisible.
   */
  reason?: string;
}

/**
 * Why: the whole point of #4278 — on load, put the persisted conversation back
 * into the chat view. Kept here rather than inline in `App.svelte` so the path
 * is unit-testable against the real store with a stubbed `fetch`, instead of
 * being untestable component glue.
 * What: fetches the newest bounded page for `agentId` and seeds it into
 * `projectId`'s bucket via `hydrateMessages`, which refuses to clobber
 * anything already there. Returns what it did so a caller can decide whether
 * to offer "load earlier".
 * Test: `rehydrateChat_restores_the_persisted_conversation`,
 * `rehydrateChat_is_a_noop_when_nothing_is_persisted`,
 * `rehydrateChat_does_not_clobber_a_message_typed_during_bootstrap`.
 */
const historyRequests = new Map<string, number>();
const historyEnds = new Map<string, number>();
const savedCursors = new Map<string, { anchor: Message; cursor: ChatHistoryCursor }>();
function rememberCursor(key: string, cursor: ChatHistoryCursor) {
  const anchor = get(messages).get(key)?.[0];
  if (anchor) savedCursors.set(key, { anchor, cursor });
}
function cachedCursor(key: string): ChatHistoryCursor | null {
  const saved = savedCursors.get(key);
  return saved && get(messages).get(key)?.includes(saved.anchor) ? saved.cursor : null;
}

export async function rehydrateChat(
  agentId: string,
  speaker: string,
  projectId: string,
  limit = DEFAULT_HISTORY_LIMIT,
): Promise<RehydrateResult> {
  const key = conversationKey(projectId, agentId);
  const generation = (historyRequests.get(key) ?? 0) + 1;
  historyRequests.set(key, generation);
  const selected = () => get(activeConversationKey) === key;
  const cached = cachedCursor(key);
  if (cached) {
    if (selected()) chatHistoryCursor.set(cached);
    return { seeded: 0, hasMore: cached.hasMore };
  }
  if (selected()) chatHistoryCursor.set(null);
  const page = await fetchChatHistory(agentId, limit);
  if (historyRequests.get(key) !== generation) return { seeded: 0, hasMore: cachedCursor(key)?.hasMore ?? false };
  if (page.available) historyEnds.set(key,page.start+page.messages.length);
  else if(page.session_absent)historyEnds.set(key,0);
  if (!page.available || page.messages.length === 0) {
    if (selected()) chatHistoryCursor.set(null);
    return { seeded: 0, hasMore: false, reason: page.reason };
  }
  const restored = historyToMessages(page, speaker, Date.now());
  const seeded = hydrateMessages(key, restored);
  if (seeded) rememberCursor(key, { agentId, speaker, projectId, start: page.start, hasMore: page.has_more });
  const cursor = cachedCursor(key);
  if (selected()) chatHistoryCursor.set(cursor);
  return { seeded: seeded ? restored.length : 0, hasMore: cursor?.hasMore ?? false };
}

/**
 * Why: the owner's volume requirement pairs a bounded initial load with lazy
 * loading of older turns on demand — without this, a bounded load is just
 * truncation. Driven by `ChatView`'s "Load earlier messages" control, which
 * renders off `canLoadOlderChat`.
 * What: takes no target — the cursor names the agent, the speaker, and the
 * bucket, so this cannot page one conversation into another's view. Gated on
 * `canLoadOlderChat`, so a cursor left over from a different agent or project
 * is refused here as well as hidden in the view. Fetches the page ending at the
 * cursor's absolute index, PREPENDS it so older turns land above what is
 * already rendered, and advances the cursor. Ids are namespaced by the page's
 * absolute `start`, so a prepended page cannot collide with the seeded one.
 * Test: `loadOlderChat_prepends_the_previous_page`,
 * `loadOlderChat_advances_the_cursor`,
 * `loadOlderChat_is_a_noop_without_a_cursor`,
 * `loadOlderChat_refuses_a_cursor_from_another_agent`,
 * `loadOlderChat_prepends_into_the_cursors_own_project`,
 * `loadOlderChat_disarms_the_cursor_at_the_start_of_history`.
 */
export async function loadOlderChat(
  limit = DEFAULT_HISTORY_LIMIT,
): Promise<RehydrateResult> {
  const cursor = get(chatHistoryCursor);
  if (!cursor || !get(canLoadOlderChat)) return { seeded: 0, hasMore: false };

  loadingOlderChat.set(true);
  try {
    const page = await fetchChatHistory(cursor.agentId, limit, cursor.start);
    if (!page.available || page.messages.length === 0) {
      const next = { ...cursor, hasMore: false };
      rememberCursor(conversationKey(cursor.projectId, cursor.agentId), next);
      if (get(chatHistoryCursor) === cursor) chatHistoryCursor.set(next);
      return { seeded: 0, hasMore: false, reason: page.reason };
    }
    const older = historyToMessages(page, cursor.speaker, Date.now());
    // The cursor's own bucket, never the currently-active one — the two can
    // differ, and prepending into the active one is how a restored conversation
    // leaks into an unrelated view.
    prependMessages(conversationKey(cursor.projectId, cursor.agentId), older);
    const next = { ...cursor, start: page.start, hasMore: page.has_more };
    rememberCursor(conversationKey(cursor.projectId, cursor.agentId), next);
    if (get(chatHistoryCursor) === cursor) chatHistoryCursor.set(next);
    return { seeded: older.length, hasMore: page.has_more };
  } finally {
    loadingOlderChat.set(false);
  }
}

/** Append completed incoming event turns without replacing live drafts or task messages. */
export async function refreshEventHistory(agentId:string,speaker:string,projectId:string):Promise<number> {
  const key=conversationKey(projectId,agentId);
  const end=historyEnds.get(key);
  const pages:ChatHistoryPage[]=[];
  let page=await fetchChatHistory(agentId,500);
  if(!page.available)throw new Error(page.reason??'Chat history is temporarily unavailable');
  const latestEnd=page.start+page.messages.length;
  if(end===undefined){
    if(get(isRunning))throw new Error('Incoming history refresh waits for the active task');
    const restored=historyToMessages(page,speaker,Date.now());
    const existingIds=new Set((get(messages).get(key)??[]).map(m=>m.id));
    const missing=restored.filter(m=>!existingIds.has(m.id));
    prependMessages(key,missing);
    historyEnds.set(key,latestEnd);
    const cursor={agentId,speaker,projectId,start:page.start,hasMore:page.has_more};
    rememberCursor(key,cursor);
    if(get(activeConversationKey)===key)chatHistoryCursor.set(cursor);
    return missing.length;
  }
  pages.push(page);
  while(page.start>end && page.has_more && pages.length<4){
    page=await fetchChatHistory(agentId,500,page.start);
    if(!page.available)throw new Error(page.reason??'Chat history is temporarily unavailable');
    pages.unshift(page);
  }
  if(page.start>end && page.has_more)throw new Error('Incoming history is too large to refresh safely');
  const restored=pages.flatMap(p=>historyToMessages(p,speaker,Date.now()));
  const groups:Message[][]=[];let group:Message[]=[];let safeEnd=latestEnd;
  const finish=(tail=false)=>{if(group[0]?.eventId){if(group.some(m=>m.role==='assistant'))groups.push(group);else if(tail)safeEnd=Math.min(safeEnd,Number(group[0].id.slice('history-'.length)));}group=[];};
  for(const message of restored){if(message.role==='event'){finish();group=[message];}else if(message.role==='user'){finish();}else if(group.length){group.push(message);}}
  finish(true);
  if(get(isRunning))throw new Error('Incoming history refresh waits for the active task');
  let added=0;
  messages.update(map=>{
    const list=map.get(key)??[];const next=[...list];const ids=new Set(next.map(m=>m.id));
    for(const rows of groups){const eventId=rows[0].eventId!;
      for(const row of rows){const index=Number(row.id.slice('history-'.length));if(index<end || ids.has(row.id))continue;
        next.push({...row,eventId});ids.add(row.id);added++;
      }
    }
    if(!added)return map;const updated=new Map(map);updated.set(key,next);return updated;
  });
  historyEnds.set(key,Math.max(historyEnds.get(key)??0,safeEnd));
  return added;
}
