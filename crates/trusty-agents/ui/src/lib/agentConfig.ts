// Agent config API client + declarative scaffolding shapes for the
// gear-panel config form (#3819, epic #3052; five-section reshape #3932).
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
// the (not yet implemented) listener spec, rendered as honest scaffolding;
// and the DOC-57 Phase-1 derivations the five-section panes read from
// already-served data (`matchesToolGlob`, `KNOWLEDGE_MCP_ENDPOINTS`) plus
// `fetchAgentSkills` — the resolved one-skill-per-tool catalog (#3933).
// Test: `agentConfig.test.ts`.
// Spec: docs/specs/agent-config-five-sections.md (DOC-57) §5 (skills, with
// OQ-2 resolved to 1:1 granularity), §4.4 (MCP knowledge connections),
// §8.5 (Phase-1 data mapping).

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
  /**
   * Mirrors the server's `PatchAgentRequest.tools_allow`. Unused by the GUI
   * since #3932: the Tools textarea was replaced by the read-only Skills pane
   * (DOC-57 §8.2/G-3), and capability editing returns as
   * `PATCH { skills_allow }` in Phase 3 (§5.7, S-12). Kept because this type
   * documents the route's body, not the panel's current usage of it.
   */
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
 * no backend yet — Bob explicitly asked for the two DEFINED listeners
 * (gmail, google-calendar) rendered as structured, honestly-scaffolded
 * bindings rather than lorem-ipsum placeholders, "so it's honest scaffolding
 * that will become live." This is UI-only data: no fetch, no persistence.
 *
 * It is CONNECTOR DEFINITIONS, never an agent's `[[listeners]]` bindings, and
 * DOC-57 §6.2 (L-1) is explicit that rendering it as per-agent state is a
 * defect: the pane used to stamp a hardcoded "not bound" badge on both
 * entries regardless of configuration, which both invents a binding that may
 * not exist and hides one that is quietly failing. #3932 therefore drops the
 * badge and labels the pane as UI scaffolding; `GET /api/agents/:name/listeners`
 * (#3937 / #3891) replaces this constant outright (C-05.1).
 * What: `id`/`label` identify the connector; `eventTypes` are the event kinds
 * a per-agent binding could filter to.
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

// ---------------------------------------------------------------------------
// DOC-57 Phase-1 derivations (#3932)
//
// Everything below turns data the sidecar ALREADY serves (`AgentDetail`) into
// the five-section view. Phase 1 adds no HTTP route and no config-schema
// change (DOC-57 §8.5, G-7); `GET …/skills` (§5.7), `GET …/knowledge` (§4.5)
// and `GET …/permissions` (§7.4) replace these derivations in Phases 2-4.
// ---------------------------------------------------------------------------

/**
 * Why: `[tools].allow` is a glob allow-list, and the Knowledge pane has to
 * answer "does this agent hold the store-bound knowledge tool?" from those
 * patterns. Re-deriving the matcher client-side is the only option in a phase
 * with no resolution route — so it mirrors the server's matcher exactly rather
 * than inventing a fourth glob dialect (DOC-57 §5.2, S-1).
 * What: Ports `match_any_glob` (`ctrl/pm_task/helpers.rs:146`): exact equality,
 * or a trailing `*` matched as a prefix. No other wildcard position is
 * supported, there and here.
 * Test: `matchesToolGlob_matches_exact_names`,
 * `matchesToolGlob_matches_trailing_wildcard_prefixes`.
 */
export function matchesToolGlob(pattern: string, tool: string): boolean {
  if (pattern.endsWith('*')) return tool.startsWith(pattern.slice(0, -1));
  return pattern === tool;
}

/**
 * Why (DOC-57 §4.3): `vector_search`'s default index is the agent's own store
 * binding (`persona.rs:346-355`, the #3864 fix), and the spec requires the
 * Knowledge pane show that linkage rather than leave the tool as a
 * free-floating chip. It is named here as the ONE store-bound knowledge tool
 * the spec states normatively — deliberately not §4.3's broader illustrative
 * tool list, which the same section marks "illustrative, not normative" and
 * warns against hardcoding, because a hardcoded taxonomy drifts exactly the
 * way the removed static `gworkspace` tool list drifted. Authoritative
 * classification arrives with `kind = "knowledge"` skill manifests in Phase 2
 * (#3933); until then the pane says so instead of guessing.
 * What: Tool names whose availability the Knowledge pane resolves against the
 * agent's own `[tools].allow`.
 * Test: `storeBoundKnowledgeTools_is_the_vector_search_seam`.
 */
export const STORE_BOUND_KNOWLEDGE_TOOLS = ['vector_search'] as const;

/**
 * One MCP/OpenRPC endpoint that backs a knowledge service (DOC-57 §4.4, K-c).
 *
 * `enabled` is the SHIPPED DEFAULT from
 * `crates/trusty-agents/assets/config/default-config.toml`, not a probe of the
 * running harness — see `KNOWLEDGE_MCP_ENDPOINTS`.
 */
export interface KnowledgeEndpoint {
  name: string;
  description: string;
  enabled: boolean;
  scopes: string[];
  /** Why the endpoint is not carrying traffic. Non-null iff `!enabled`. */
  reason?: string;
}

/**
 * Why (DOC-57 §8.5): Phase 1 maps the Knowledge pane's K-c sub-surface onto "a
 * read-only, statically-declared MCP knowledge-endpoint list", because no
 * route exposes live MCP connection state yet. §4.4 is emphatic about WHAT
 * such a list must say: `trusty-memory` and `trusty-search` used to ship as
 * `[[tool_registry.endpoints]]` OpenRPC entries with `enabled = false`
 * (awaiting a `--rpc` stdio mode neither binary ever implemented — genuinely
 * dead config, not merely disabled-by-default). Both binaries already
 * implement the generic MCP-stdio protocol trusty-agents uses for
 * `trusty-mpm`/`granola-notes`, so the fix moved them to live
 * `[[mcp.services]]` entries (`enabled = true`, `discover = true`) instead of
 * waiting on a driver that will never ship. Rendering a
 * configured-but-disabled endpoint as connected — or a configured-and-enabled
 * one as disabled — is the same class of defect as the fabricated listener
 * pane (C-03.2).
 * What: The three knowledge endpoints declared in this crate's shipped
 * `assets/config/default-config.toml` `[[mcp.services]]` section, mirroring
 * their `enabled` flags. This is the DEFAULT declaration; an operator who
 * has edited `~/.trusty-agents/config.toml` may have flipped a flag, which is
 * why the pane labels the list as declared-not-probed and Phase 3's
 * `GET /api/agents/:name/knowledge` (§4.5) replaces it with resolved state.
 * Test: `knowledgeEndpoints_report_the_shipped_defaults`.
 */
export const KNOWLEDGE_MCP_ENDPOINTS: KnowledgeEndpoint[] = [
  {
    name: 'trusty-memory',
    description: 'Trusty memory service — recall/remember/forget over a native MCP-stdio server',
    enabled: true,
    scopes: ['memory.read', 'memory.write'],
  },
  {
    name: 'trusty-search',
    description:
      'Trusty search service — semantic + keyword code/knowledge search over a native MCP-stdio server',
    enabled: true,
    scopes: ['search.read'],
  },
  {
    name: 'gworkspace',
    description: 'Google Workspace — Gmail, Calendar, Drive, Docs, Sheets, Tasks',
    enabled: true,
    scopes: ['google.*'],
  },
];

/**
 * One resolved skill card from `GET /api/agents/:name/skills` (#3933).
 *
 * Owner decision (2026-07-25, resolving DOC-57 OQ-2): **one skill per tool**,
 * each with a human, provider-recognisable name — "MTA Train Time", not
 * `get_train_schedule`. `tools` therefore holds exactly zero or one entry; a
 * zero-entry skill is a first-class TOOL-LESS skill (guidance with no
 * executable member), not a broken card.
 */
export interface AgentSkill {
  /** Stable id — the unit `[skills].allow` names. */
  id: string;
  /** Human name. Never an echo of the tool identifier. */
  name: string;
  /** One line on what invoking it accomplishes. Empty for a derived skill. */
  description: string;
  /**
   * `function` (#4022) is a BUNDLE — a group header over its member cards, not
   * a capability of its own. It wraps no tool, so a pane that buckets by kind
   * must not drop it into the Action bucket; rendering the group is #4024's job.
   */
  kind: 'action' | 'knowledge' | 'system' | 'function';
  /** `builtin` | `authored` (with `path`) | `derived`. */
  origin: { kind: string; path?: string };
  /**
   * Whether THIS agent is granted the skill, per the real dispatch gate. For a
   * `function` card this is the conservative reading of the tri-state below —
   * `granted_state === 'all'`.
   */
  granted: boolean;
  /** The wrapped tool — zero or one name (1:1). Empty for a bundle. */
  tools: string[];
  /** Credential this skill needs, when it needs one. */
  provider: AgentSkillProvider | null;
  /** #4022 member skill ids for a `function` card; empty for a leaf. */
  members: string[];
  /** The subset of `members` this agent holds; empty for a leaf. */
  granted_members: string[];
  /**
   * #4025 tri-state. For a bundle: `all`/`some`/`none` of its members. For a
   * leaf it simply restates `granted`, so no kind check is needed to read it.
   */
  granted_state: 'all' | 'some' | 'none';
}

/**
 * One function-skill group from `GET /api/agents/:name/skills` (#4025).
 *
 * A convenience index over the `kind: 'function'` cards in `skills[]` — the
 * server builds both from ONE computation, so a group and its card can never
 * report different grant states. Members are skill ids to be looked up in
 * `skills[]`; a member id the catalog cannot resolve is still LISTED here (it is
 * what the bundle declares) but can never appear in `granted_members`.
 */
export interface AgentSkillGroup {
  id: string;
  name: string;
  description: string;
  members: string[];
  granted_members: string[];
  granted_state: 'all' | 'some' | 'none';
  /**
   * #4024: the DISTINCT credential requirements across the members — a SET, not
   * a rolled-up verdict. A bundle has no `provider` of its own, and its members
   * need not agree; render every entry, so two members needing two different
   * credentials show as two chips rather than one averaged claim. Empty means
   * "no member states a requirement", never "verified". Optional so an older
   * sidecar (pre-#4024) renders the group rather than throwing.
   */
  providers?: AgentSkillProvider[];
}

/**
 * A skill's provider/credential requirement.
 *
 * `configured` is deliberately tri-state: `true`/`false` when an environment
 * variable backs the credential and the sidecar can read it, and `null` when
 * the credential is an OAuth grant or MCP wiring the route does NOT verify.
 * Render `null` as "not verified" — never as a green check, which would assert
 * a check nobody ran.
 */
export interface AgentSkillProvider {
  provider: string;
  requirement: string;
  env_var: string | null;
  configured: boolean | null;
}

/**
 * Render one credential requirement as a chip label + tone (#4024).
 *
 * Why: Both a leaf card and a function-skill group header state credentials, and
 * they must state them IDENTICALLY — a group that renders "configured" where its
 * member card says "not verified" is the display-layer form of the drift the
 * route's single-computation rule exists to prevent. One function, two call
 * sites. It is also where `configured: null` is pinned to "not verified" rather
 * than to a green chip nobody checked (DOC-57 S-11b).
 * What: `{ label, tone, title }`; `tone` is `ok` | `bad` | `unknown`.
 * Test: `agentConfig.test.ts`, `AgentConfigSkills.test.ts`.
 */
export function providerChip(p: AgentSkillProvider): {
  label: string;
  tone: 'ok' | 'bad' | 'unknown';
  title: string;
} {
  if (p.configured === true)
    return { label: `${p.provider} configured`, tone: 'ok', title: p.requirement };
  if (p.configured === false)
    return {
      label: `${p.provider} NOT configured`,
      tone: 'bad',
      title: `${p.requirement} Set ${p.env_var}.`,
    };
  return { label: `${p.provider} — not verified`, tone: 'unknown', title: p.requirement };
}

/** `GET /api/agents/:name/skills`'s wire shape. */
export interface AgentSkills {
  skills: AgentSkill[];
  /** Granted CAPABILITIES — `function` bundles are excluded (#4025). */
  granted_count: number;
  /** #4025 function-skill groups, indexing the `kind: 'function'` cards. */
  groups: AgentSkillGroup[];
  /** `[skills].allow` ids that resolved to nothing (DOC-57 S-11). */
  unresolved: { id: string; reason: string }[];
  /** Allow-patterns no catalog tool matches — may still resolve over MCP. */
  unmatched_patterns: { pattern: string; reason: string }[];
  /**
   * `[tools].scopes` patterns that can match NO reachable dotted scope
   * (#3987). Unlike `unmatched_patterns` — which may still resolve to a
   * live-discovered MCP tool — these are conclusively broken: every scoped
   * tool the grant was meant to permit is denied at dispatch, which looks
   * exactly like "this agent has no tools". `nearest` carries working
   * alternatives in the same namespace, so the pane can be actionable rather
   * than merely alarming. Render it as a WARNING, not as an aside.
   */
  dead_scope_patterns: { pattern: string; nearest: string[]; reason: string }[];
  /** `false` when the agent declares no capability at all (grants nothing). */
  declares_capability: boolean;
  config_error?: string;
}

/**
 * Why: The Skills pane's whole value is showing RESOLVED capability. Phase 1
 * had no route, so it grouped allow-list globs by their `_` prefix and badged
 * every card synthetic; that showed the user prefix fragments and could not say
 * whether anything resolved. This is the route that replaces the guess.
 * What: `null` on 404 (unknown agent) so a stale roster selection is a normal
 * outcome, not a thrown error — same contract as `fetchAgentStores`.
 * Test: `fetchAgentSkills_returns_null_on_404`.
 */
export async function fetchAgentSkills(name: string): Promise<AgentSkills | null> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}/skills`, {
    headers: authHeaders(),
  });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET /api/agents/${name}/skills failed: ${r.status}`);
  return (await r.json()) as AgentSkills;
}

/**
 * One in-product delegation target from `GET /api/agents/:name/subagents`
 * (#4029) — a trusty-agents agent this agent may hand a task to via
 * `delegate_to_agent`.
 *
 * `reachable` already accounts for ALL THREE refusals the server applies —
 * ADR-0024's delegation KIND rule (assistant → assistant is refused, read from
 * roles, not tiers), #4169's one-directional L0/L1 tier gate, and ADR-0024
 * decision 4's per-agent reachable-set whitelist — so a pane must render
 * `reachable === false` as DENIED with its `reason` and must never present the
 * card as callable. The server refuses to list targets the gate
 * would refuse; a card that arrives unreachable is one the operator needs an
 * explanation for, not one to hide.
 */
export interface SubagentInProductTarget {
  name: string;
  display_name: string;
  /** The target's resolved `[agent].role` — the field the allowlist gates on. */
  role: string;
  /** `l0` | `l1`, resolved fail-closed server-side. */
  tier: string;
  reachable: boolean;
  /** Present iff `reachable === false`. Explains which gate refused it. */
  reason: string | null;
}

/**
 * The in-product half of the Sub-agents section: `delegate_to_agent`.
 *
 * `tool_registered` and `tool_granted` are deliberately separate. The tool is
 * only registered for assistant-tier personas, and even then dispatch is gated
 * by the agent's own resolved tool patterns — collapsing the two into one badge
 * would report "can delegate" for an agent that holds no such grant.
 */
export interface SubagentInProduct {
  mechanism: 'in_product';
  tool: string;
  tool_registered: boolean;
  tool_granted: boolean;
  /** THIS agent's own resolved tier — the left-hand side of the gate. */
  delegator_tier: string;
  /**
   * `ASSISTANT_ALLOWED_DELEGATE_ROLES`, verbatim — the COARSE, pre-kind-gate
   * role eligibility filter, NOT the reachable set. It still contains
   * `assistant` (deliberately kept in the Rust constant, whose doc explains
   * why this report is one of the consumers holding it there), yet the kind
   * rule refuses every assistant→assistant edge. A pane must therefore not
   * present this array as "the targets": state the effective rule
   * (role-eligible MINUS kind-refused) and read reachability off
   * `targets[].reachable`, which is the only field that carries both gates.
   */
  allowed_roles: string[];
  /**
   * ADR-0024 decision 4: whether the reachable-set whitelist gate applies to
   * THIS agent. It applies to the assistant kind and to nobody else — `pm`/
   * `ctrl`-as-orchestrator delegate through separately-trusted paths the rule
   * does not revisit. False here means `reachable` carries only the kind and
   * tier gates.
   */
  whitelist_enforced: boolean;
  /**
   * Whether this agent declares `[subagents].delegate_allowed` at all. `false`
   * does NOT mean "everything" — it means NOTHING is reachable (fail-closed,
   * the owner's ratified default). A pane must say so explicitly, exactly as
   * the cross-product half already does for `declares_allowed`.
   */
  declares_whitelist: boolean;
  /**
   * `ASSISTANT_REACHABLE_SUBAGENTS` — the server-owned floor the editable
   * whitelist narrows. Reported even when `whitelist_enforced` is false, so a
   * pane can state the ceiling for any viewer. Configuration can narrow this,
   * never widen it: a write naming anything outside it is refused by
   * `PATCH /api/agents/:name` and, behind that, by the dispatch gate.
   */
  reachable_floor: string[];
  targets: SubagentInProductTarget[];
  /** Roster entries excluded by the role allowlist — counted, never named. */
  role_excluded_count: number;
  /**
   * Roster entries excluded because they carry `[agent].hidden = true` (#4235)
   * — counted, never named, exactly like `role_excluded_count`. Disjoint from
   * it: the server checks `hidden` first, so an entry contributes to one
   * counter or the other, never both. Rendered as its own line rather than
   * summed with the role count, because the two have different remedies (edit
   * the agent's `hidden` flag vs. its `role`).
   */
  hidden_excluded_count: number;
  unresolved: { name: string; reason: string }[];
}

/**
 * The cross-product half: `dispatch_task` into trusty-code's non-coding roster.
 *
 * Every `bridge_floor` specialist appears in `targets` with its grant state
 * (the `/skills` precedent — report the whole vocabulary, not only the granted
 * subset), so a denied specialist carries a reason rather than being invisible.
 * `rejected` holds declared names the bridge floor hard-denies, which is the one
 * way a hand-written `[subagents].allowed` silently does nothing.
 */
export interface SubagentCrossProduct {
  mechanism: 'cross_product';
  tool: string;
  tool_granted: boolean;
  /** `false` when the agent declares no `[subagents]` section at all. */
  declares_allowed: boolean;
  /** `NON_CODING_TARGETS` — the bridge-owned floor config can only narrow. */
  bridge_floor: string[];
  targets: { name: string; granted: boolean; reason: string | null }[];
  rejected: { name: string; reason: string }[];
}

/** `GET /api/agents/:name/subagents`'s wire shape (#4029). */
export interface AgentSubagents {
  /**
   * `false` when the agent's own config could not be resolved. Both halves then
   * arrive empty and deny-everything, with `config_error` set — fail-closed, not
   * a partial-parse guess.
   */
  resolved: boolean;
  in_product: SubagentInProduct;
  cross_product: SubagentCrossProduct;
  config_error?: string;
}

/**
 * Why (#4029, epic #4021 OQ-5): neither delegation mechanism was reachable over
 * HTTP — `parse_agent_toml` enumerates twelve fields and `subagents` is not one
 * of them — so the Sub-agents pane needs its own route rather than a derivation
 * from `AgentDetail`. Critically, the in-product target set is not config at
 * all: it is a hardcoded role allowlist crossed with the resolvable roster,
 * then narrowed by ADR-0024's delegation kind rule and #4169's tier gate, none
 * of which a client can compute.
 * What: `null` on 404 (unknown agent) so a stale roster selection is a normal
 * outcome, not a thrown error — same contract as `fetchAgentSkills`.
 * Test: `fetchAgentSubagents_returns_null_on_404`.
 */
export async function fetchAgentSubagents(name: string): Promise<AgentSubagents | null> {
  const r = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(name)}/subagents`, {
    headers: authHeaders(),
  });
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`GET /api/agents/${name}/subagents failed: ${r.status}`);
  return (await r.json()) as AgentSubagents;
}
