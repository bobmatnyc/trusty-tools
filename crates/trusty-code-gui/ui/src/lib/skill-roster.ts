// Why: issue #3449 — the Foundry GUI's Skills tab needs the daemon's full
// skill catalog (bundled ∪ project-disk, `crate::skills::protocol`) plus
// add/remove over the project-disk tier. Mirrors `lib/agent-roster.ts`'s
// shape exactly — the two tabs are siblings — but the wire values differ:
// tcode's skill model recognises exactly TWO tiers (`"bundled"` and
// `"project"` — NEVER `"user"`; see `crate::skills::protocol`'s module docs
// for why there is no user-level skills directory the way agents has one),
// so `SkillCatalogEntry['tier']` is narrower than `AgentCatalogEntry['tier']`
// on purpose, not an oversight.
//
// What: `SkillCatalogEntry` mirrors `crate::skills::protocol::entry_json`'s
// wire shape. `fetchSkillRoster`/`createSkill`/`deleteSkill` are thin
// `fetch()` wrappers over the unprefixed `/skills*` REST routes
// (`crate::serve::rest::skill_catalog`).
//
// Test: `skill-roster.test.ts`.

/** Mirrors `crate::skills::protocol`'s per-entry wire shape. */
export interface SkillCatalogEntry {
  name: string;
  tier: 'bundled' | 'project';
  description: string;
}

/** `GET /skills` response envelope. */
export interface SkillCatalogResponse {
  skills: SkillCatalogEntry[];
}

/**
 * Parse a REST error envelope's `error.message`, falling back to a bare HTTP
 * status when the body is missing/malformed — mirrors
 * `lib/agent-roster.ts::agentApiErrorMessage`.
 * Test: `skill-roster.test.ts::skillApiErrorMessage`.
 */
export async function skillApiErrorMessage(res: Response): Promise<string> {
  const body = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
  return body?.error?.message ?? `HTTP ${res.status}`;
}

/**
 * `GET /skills` — the full bundled ∪ project-disk skill catalog. Works
 * projectless too (bundled-only), matching `crate::skills::protocol::skills_list`.
 * Test: `skill-roster.test.ts::fetchSkillRoster`.
 */
export async function fetchSkillRoster(
  base: string,
  signal?: AbortSignal,
): Promise<SkillCatalogEntry[]> {
  const res = await fetch(`${base}/skills`, { signal });
  if (!res.ok) throw new Error(await skillApiErrorMessage(res));
  const body = (await res.json()) as SkillCatalogResponse;
  return body.skills;
}

/**
 * `POST /skills` — create a project-disk skill from a Markdown+frontmatter
 * `SKILL.md` body. Throws with the daemon's error message on any non-`201`
 * (e.g. a `400` when projectless, a `403` bundled-name collision, a `409`
 * already-exists conflict).
 * Test: `skill-roster.test.ts::createSkill`.
 */
export async function createSkill(
  base: string,
  name: string,
  content: string,
): Promise<SkillCatalogEntry> {
  const res = await fetch(`${base}/skills`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, content }),
  });
  if (res.status !== 201) throw new Error(await skillApiErrorMessage(res));
  return (await res.json()) as SkillCatalogEntry;
}

/**
 * `DELETE /skills/{name}` — remove a project-disk skill. Throws with the
 * daemon's error message on any non-`200`.
 * Test: `skill-roster.test.ts::deleteSkill`.
 */
export async function deleteSkill(base: string, name: string): Promise<void> {
  const res = await fetch(`${base}/skills/${encodeURIComponent(name)}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await skillApiErrorMessage(res));
}
