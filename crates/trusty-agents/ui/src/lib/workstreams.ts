// Workstreams sidebar API client + agent-grouping (#3819, epic #3052).
//
// Why: the sidebar's `WORKSTREAMS` section lists workstreams sourced from
// `GET /api/workstreams` (trusty-memory's `ws:<name>` tag convention, DOC-53
// — see the Rust module `api/server/workstreams.rs` for the full design
// note) and groups them by owning agent per Bob's directive. DOC-53 tags a
// workstream by NAME, not by which persona/agent was active — there is no
// `agent:<name>` companion tag today — so `groupByAgent` is an HONEST
// best-effort heuristic (name-prefix match against the known roster ids,
// falling back to an "Other" bucket), not a real attribute read from the
// backend. Documented here and in the PR body rather than silently assumed
// accurate.
// What: `fetchWorkstreams`/`fetchWorkstreamHistory` — thin REST wrappers.
// `groupByAgent` — pure, testable grouping.
// Test: `workstreams.test.ts` covers `groupByAgent`.

import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

export interface WorkstreamSummary {
  name: string;
  last_activity: string;
  summary: string;
  has_open_claim: boolean;
  areas: string[];
  item_count: number;
}

export interface WorkstreamHistoryItem {
  content: string;
  created_at: string;
  tags: string[];
}

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/** `GET /api/workstreams`. Returns `[]` on any failure — a sidebar
 * convenience panel must never surface a network error as a crash. */
export async function fetchWorkstreams(): Promise<WorkstreamSummary[]> {
  try {
    const r = await fetch(`${apiBase()}/api/workstreams`, { headers: authHeaders() });
    if (!r.ok) return [];
    return (await r.json()) as WorkstreamSummary[];
  } catch {
    return [];
  }
}

/** `GET /api/workstreams/:name/history`. Returns `[]` on any failure. */
export async function fetchWorkstreamHistory(
  name: string,
  limit = 20,
): Promise<WorkstreamHistoryItem[]> {
  try {
    const r = await fetch(
      `${apiBase()}/api/workstreams/${encodeURIComponent(name)}/history?limit=${limit}`,
      { headers: authHeaders() },
    );
    if (!r.ok) return [];
    return (await r.json()) as WorkstreamHistoryItem[];
  } catch {
    return [];
  }
}

export interface AgentGroup {
  agentId: string;
  agentLabel: string;
  workstreams: WorkstreamSummary[];
}

const OTHER_BUCKET = 'other';
const OTHER_LABEL = 'Other';

/**
 * Why: DOC-53's `ws:<name>` tag carries no agent identity — see module doc.
 * A workstream branch name commonly embeds the agent that drove it (e.g.
 * `feat-izzie-weather-metronorth`, `fix-cto-assistant-...`) by repo
 * convention, so a best-effort match against the known roster ids/labels is
 * the cheapest honest signal available without a backend change. Anything
 * that doesn't match lands in the "Other" bucket rather than being guessed
 * into a wrong agent.
 * What: Groups `workstreams` by the first roster id (from `knownAgentIds`,
 * lowercased) found as a substring of the workstream's `name`. Groups are
 * ordered: matched agents in `knownAgentIds` order, then "Other" last. Each
 * group's workstreams stay sorted by `last_activity` desc (the input order,
 * since `GET /api/workstreams` already returns them that way).
 * Test: `groupByAgent_matches_known_agent_by_name_substring`,
 * `groupByAgent_falls_back_to_other`,
 * `groupByAgent_preserves_workstream_order_within_group`.
 */
export function groupByAgent(
  workstreams: WorkstreamSummary[],
  knownAgents: { id: string; label: string }[],
): AgentGroup[] {
  const groups = new Map<string, AgentGroup>();
  const order: string[] = [];

  for (const agent of knownAgents) {
    groups.set(agent.id, { agentId: agent.id, agentLabel: agent.label, workstreams: [] });
    order.push(agent.id);
  }
  groups.set(OTHER_BUCKET, { agentId: OTHER_BUCKET, agentLabel: OTHER_LABEL, workstreams: [] });
  order.push(OTHER_BUCKET);

  for (const ws of workstreams) {
    const lowerName = ws.name.toLowerCase();
    const match = knownAgents.find((a) => a.id && lowerName.includes(a.id.toLowerCase()));
    const bucket = groups.get(match?.id ?? OTHER_BUCKET);
    bucket?.workstreams.push(ws);
  }

  return order
    .map((id) => groups.get(id))
    .filter((g): g is AgentGroup => !!g && g.workstreams.length > 0);
}
