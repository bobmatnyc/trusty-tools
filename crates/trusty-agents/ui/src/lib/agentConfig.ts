// Agent config API client + declarative scaffolding shapes for the
// gear-panel config form (#3819, epic #3052).
//
// Why: the chat-pane gear panel needs a single typed surface over the agent
// config read/write routes (`GET/PATCH /api/agents/:name`,
// `GET /api/agents/:name/persona`) plus the LIVE listener/OKG-store shape
// Bob asked the concept demo to render even though there is no backend for
// either yet (issue being filed for listeners; OKG store bindings have no
// `agent.toml` field today — see `DEFINED_LISTENERS`/`describeOkgStores`'s
// doc comments). Keeping the fetch/patch logic here (not inline in the
// component) makes it independently testable, mirroring `stores/app.ts`'s
// `fetchAgentCatalog` pattern.
// What: `fetchAgentDetail`/`fetchAgentPersona`/`patchAgent` — thin REST
// wrappers; `DEFINED_LISTENERS` — the two listener bindings from the (not
// yet implemented) listener spec, rendered as honest scaffolding; a
// `template-scaffolded-agent.toml`-shaped starter for the add-agent flow.
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
 * Why: "OKG Stores" (the agent's bound knowledge trees + search indexes,
 * e.g. Izzie's `bob-kb`) has no representation in `agent.toml` today —
 * confirmed by grep against `crates/trusty-agents/src/agents/config.rs`
 * before choosing to scaffold rather than fabricate a live reading. Per
 * Bob's own escape valve ("render... as a structured placeholder... state
 * in the PR body which sections are live vs scaffolded"), this returns a
 * fixed, clearly-labeled scaffold shape rather than fake doc/chunk counts —
 * inventing believable-looking numbers here would be fabricated data
 * (BASE-ENGINEER "No Mock Data" — mock data belongs in test code only).
 * What: One scaffold row so the section isn't empty in the concept demo;
 * `connected: false` and the UI renders it as "not yet connected" rather
 * than implying a live reading.
 */
export interface OkgStoreBinding {
  name: string;
  tree: string;
  index: string;
  connected: boolean;
}

export function scaffoldOkgStores(agentName: string): OkgStoreBinding[] {
  return [
    {
      name: `${agentName}-kb`,
      tree: `okg://${agentName}`,
      index: `trusty-search:${agentName}`,
      connected: false,
    },
  ];
}
