// Agent config API client + declarative scaffolding shapes for the
// gear-panel config form (#3819, epic #3052).
//
// Why: the chat-pane gear panel needs a single typed surface over the agent
// config read/write routes (`GET/PATCH /api/agents/:name`,
// `GET /api/agents/:name/persona`), the LIVE OKG-store bindings
// (`GET /api/agents/:name/stores`, #3816/#3864), and the listener shape Bob
// asked the concept demo to render while its backend is still being built
// (see `DEFINED_LISTENERS`' doc comment). Keeping the fetch/patch logic here
// (not inline in the component) makes it independently testable, mirroring
// `stores/app.ts`'s `fetchAgentCatalog` pattern.
// What: `fetchAgentDetail`/`fetchAgentPersona`/`patchAgent`/`fetchAgentStores`
// — thin REST wrappers; `DEFINED_LISTENERS` — the two listener bindings from
// the (not yet implemented) listener spec, rendered as honest scaffolding.
// Test: `agentConfig.test.ts`.

import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/** `GET/PATCH /api/agents/:name`'s wire shape (parse_agent_toml's JSON). */
export interface AgentDetail {
  name: string;
  role: string;
  model: string;
  runner: string;
  provider_id: string;
  description: string;
  display_name: string;
  /** `[tools].allow` — the glob-style persona tool-allowlist. */
  tools_allow: string[];
  /** `[tools].scopes` — RBAC/OpenRPC scope patterns. Read-only in this slice. */
  scopes: string[];
  /** `[agent].hidden` — excluded from picker/roster listings (#3819). */
  hidden: boolean;
}

/** `GET /api/agents/:name/persona`'s wire shape. */
export interface AgentPersona {
  content: string | null;
  /** `false` for flat (non-package) agents — no persona.md to write. */
  editable: boolean;
}

/**
 * Why: A 404 (unknown agent) is a normal, expected outcome for a stale
 * roster selection — callers branch on `null`, not a caught exception.
 * What: `GET /api/agents/:name`. Throws on any non-404 network/HTTP error.
 * Test: `fetchAgentDetail_returns_null_on_404`.
 */
export async function fetchAgentDetail(name: string): Promise<AgentDetail | null> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}`, {
    headers: authHeaders(),
  });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET /api/agents/${name} failed: ${r.status}`);
  return (await r.json()) as AgentDetail;
}

/** `GET /api/agents/:name/persona`. Throws on any non-404 network/HTTP error. */
export async function fetchAgentPersona(name: string): Promise<AgentPersona | null> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}/persona`, {
    headers: authHeaders(),
  });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET /api/agents/${name}/persona failed: ${r.status}`);
  return (await r.json()) as AgentPersona;
}

export interface PatchAgentBody {
  model_id?: string;
  provider_id?: string;
  personality?: string;
  tools_allow?: string[];
}

/** `PATCH /api/agents/:name`. Throws (with the server's `error` message when
 * present) on a non-2xx response so callers can surface it in the form. */
export async function patchAgent(name: string, body: PatchAgentBody): Promise<AgentDetail> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    let message = `PATCH /api/agents/${name} failed: ${r.status}`;
    try {
      const errBody = (await r.json()) as { error?: string };
      if (errBody.error) message = errBody.error;
    } catch {
      // Non-JSON error body — keep the generic message.
    }
    throw new Error(message);
  }
  return (await r.json()) as AgentDetail;
}

/**
 * Why (Bob, concept-demo priority): the config pane's LISTENERS section has
 * no backend yet (spec being filed) — Bob explicitly asked for the two
 * DEFINED listeners (gmail, google-calendar) rendered as structured,
 * honestly-scaffolded bindings rather than lorem-ipsum placeholders, "so
 * it's honest scaffolding that will become live." This is UI-only data: no
 * fetch, no persistence — every agent shows the same two listener
 * definitions with per-agent event-type filter chips toggled from nothing
 * (all off) until a real backend exists.
 * What: `id`/`label` identify the listener; `eventTypes` are the
 * connector's defined event kinds a per-agent binding can filter to (empty
 * `enabledEventTypes` = "not bound" — the honest default, never
 * pre-checked).
 */
export interface ListenerDefinition {
  id: 'gmail' | 'google-calendar';
  label: string;
  description: string;
  eventTypes: string[];
}

export const DEFINED_LISTENERS: ListenerDefinition[] = [
  {
    id: 'gmail',
    label: 'Gmail',
    description: 'Inbound-message events from a connected Gmail account.',
    eventTypes: ['message.received', 'thread.updated', 'label.applied'],
  },
  {
    id: 'google-calendar',
    label: 'Google Calendar',
    description: 'Event lifecycle notifications from a connected calendar.',
    eventTypes: ['event.created', 'event.updated', 'event.starting_soon'],
  },
];

/**
 * One OKG store binding, resolved LIVE by the backend (#3816/#3864).
 *
 * Why: this replaces the pre-#3816 client-side scaffold. The binding now
 * exists in `agent.toml` as `[[stores]]` and
 * `GET /api/agents/:name/stores` resolves each one against the running
 * trusty-search / trusty-memory daemons, so the card reports an OBSERVED
 * state instead of asserting "not connected" for every agent.
 * What: mirrors `crate::stores::status::StoreStatus`'s serde shape.
 * `connected` reflects the SEARCH INDEX (the corpus `vector_search` routes
 * to); `reason` is present only when it is false. Palace health is separate
 * so a missing palace downgrades one line, not the whole store. Every
 * optional field is omitted by the server when absent — never guessed.
 */
export interface OkgStoreBinding {
  name: string;
  tree: string;
  index: string;
  palace?: string;
  connected: boolean;
  reason?: string;
  chunk_count?: number;
  root_path?: string;
  index_status?: string;
  palace_connected?: boolean;
  palace_reason?: string;
}

/** `GET /api/agents/:name/stores`'s wire shape. */
export interface AgentStores {
  stores: OkgStoreBinding[];
  /** Non-fatal config problems (e.g. more than one store bound). */
  issues: string[];
  /** Present only when the agent's TOML failed to parse. */
  config_error?: string;
}

/**
 * Why: an agent that binds no store is a normal state (empty list), and a
 * daemon being down is reported per-store rather than as a request failure —
 * so the only `null` case worth branching on is a stale roster selection.
 * What: `GET /api/agents/:name/stores`. Returns `null` on 404; throws on any
 * other network/HTTP error.
 * Test: `fetchAgentStores_returns_null_on_404`,
 * `fetchAgentStores_parses_live_status`.
 */
export async function fetchAgentStores(name: string): Promise<AgentStores | null> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}/stores`, {
    headers: authHeaders(),
  });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET /api/agents/${name}/stores failed: ${r.status}`);
  return (await r.json()) as AgentStores;
}
