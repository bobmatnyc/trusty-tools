/**
 * Per-assistant MCP connections: the global tier, this assistant's overrides,
 * and the effective set (#7454, ADR-0060).
 *
 * An override is a DELTA, so `overrides` alone cannot say what an assistant
 * connects to — `global` is the list it applies to and `resolved` is the
 * answer. `statuses` runs parallel to `resolved`: a server that is configured
 * but not usable (disabled, or a credential that does not resolve) is present
 * with a reason, never omitted.
 */
import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

export type McpTier = 'global' | 'assistant';

/** The transport half of a server, kept loose — the enum is non-exhaustive. */
export interface McpTransport {
  type: string;
  command?: string;
  args?: string[];
  url?: string;
}

export interface McpServer {
  name: string;
  enabled: boolean;
  transport: McpTransport;
  extensions?: Record<string, unknown>;
}

export interface McpServerStatus {
  name: string;
  tier: McpTier;
  enabled: boolean;
  usable: boolean;
  reason: string | null;
}

export interface McpIssue {
  tier: McpTier;
  path: string;
  detail: string;
  remedy: string;
}

export interface AssistantMcp {
  assistant: string;
  /** Every server the shared file declares, before this assistant's overrides. */
  global: McpServer[];
  /** This assistant's own `[mcp]` table. */
  overrides: { servers: McpServer[]; disabled: string[] };
  /** What this assistant actually connects to. */
  resolved: McpServer[];
  /** One entry per `resolved` server, in the same order. */
  statuses: McpServerStatus[];
  /** Anything wrong with either tier's file. Never fatal — see ADR-0060. */
  issues: McpIssue[];
}

async function request(assistant: string, body?: unknown): Promise<AssistantMcp> {
  const token = getCurrentApiToken();
  const response = await fetch(`${apiBase()}/api/assistants/${encodeURIComponent(assistant)}/mcp`, {
    method: body === undefined ? 'GET' : 'PUT',
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
    },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    throw new Error(
      typeof error.error === 'string' ? error.error : `MCP request failed (${response.status})`,
    );
  }
  const result: AssistantMcp = await response.json();
  // Same guard as the memory pane: a response for a different assistant would
  // render one assistant's connections under another's name.
  if (result.assistant !== assistant) throw new Error('MCP response belongs to another assistant.');
  return result;
}

export const fetchAssistantMcp = (assistant: string) => request(assistant);

/** Replace the `[mcp]` table, keeping this assistant's own added servers. */
export const saveAssistantMcpDisabled = (
  assistant: string,
  current: AssistantMcp,
  disabled: string[],
) => request(assistant, { servers: current.overrides.servers, disabled });

/** A short, human label for where a server is reached. */
export function transportLabel(server: McpServer): string {
  if (server.transport.type === 'stdio') return server.transport.command ?? 'stdio';
  return server.transport.url ?? server.transport.type;
}
