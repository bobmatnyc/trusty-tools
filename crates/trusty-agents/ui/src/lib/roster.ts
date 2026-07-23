// Agent roster helpers (#3223, #3224 — Trusty Agents agent roster, epic
// #3052).
//
// Why: The roster needs to merge two independent data sources — the
// project's `GET /api/agents` catalog (`.trusty-agents/agents/*.toml`) and a
// user's `~/.trusty-agents/agents/*.md` personalization overlays (Tauri-only,
// via `list_personalization_overlays`) — into one deduplicated list, plus
// turn a free-text display name into a valid overlay slug for the "new
// agent" create flow. Both are pure functions (no fetch/invoke) so they're
// unit-testable without a running API server or Tauri runtime; the actual
// data fetching lives in `stores/app.ts`.
// What: `slugify`, `BASE_AGENT_ID`, `CatalogAgent`/`OverlayAgent`/`RosterEntry`
// types, `buildRoster`, plus `buildOverlayMarkdown`/`parseOverlayMarkdown` —
// the create/edit form's (#3224) markdown-frontmatter codec, mirroring the
// Rust-side `parse_frontmatter_fields` in `ui/src-tauri/src/overlay.rs` so
// the editor's preview never disagrees with what the backend actually
// parses on the next chat dispatch.
// Test: `roster.test.ts`.

/** Shape of one entry in `GET /api/agents`'s `{"agents": [...]}` envelope. */
export interface CatalogAgent {
  name: string;
  role?: string;
  model?: string;
  runner?: string;
  description?: string;
  /**
   * #3738: the persona's human-facing speaker label ("Assistant" / "Izzie" /
   * "CTO Bot"), served straight from `[agent].display_name` (falling back to
   * `name` backend-side — see `parse_agent_toml`). The roster row's `label`
   * and the per-message speaker attribution both read this so the display
   * name is never re-derived client-side.
   */
  display_name?: string;
}

/** Shape of one entry returned by the Tauri `list_personalization_overlays` command. */
export interface OverlayAgent {
  slug: string;
  name: string;
  display_name?: string | null;
  extends?: string | null;
}

export type RosterSource = 'base' | 'catalog' | 'overlay';

/** One row in the merged roster the switcher/editor render. */
export interface RosterEntry {
  /** The value threaded into `TaskRequest.agent` — a dispatchable agent name. */
  id: string;
  /** Human-friendly label for the roster row. */
  label: string;
  description?: string;
  source: RosterSource;
  /** Set for overlay entries — the base agent this overlay personalizes. */
  extends?: string;
}

/**
 * Why: The base, nameless "assistant" agent (DOC-41 §2.5.1) is always a
 * valid chat target even before the user creates any personalization
 * overlay or the project catalog lists it (it's a directory-package agent,
 * which `GET /api/agents`'s flat-`.toml`-only scan does not enumerate — see
 * `crates/trusty-agents/src/api/server/projects.rs::scan_agents_dir`). The
 * roster always shows it first so "chat with the assistant" works out of
 * the box.
 */
export const BASE_AGENT_ID = 'assistant';

/**
 * Why (#3737): the human-friendly label for the base, nameless assistant.
 * Exported so chat attribution (`rosterDisplayName`) can fall back to it
 * without re-typing the literal — the base agent has no `display_name` on the
 * wire (it's intentionally nameless), so both the roster row and any bubble
 * it produces read "Assistant".
 */
export const BASE_AGENT_LABEL = 'Assistant';

const BASE_ENTRY: RosterEntry = {
  id: BASE_AGENT_ID,
  label: BASE_AGENT_LABEL,
  description: 'General-purpose productivity assistant',
  source: 'base',
};

/**
 * Why: Overlay files are addressed by filesystem-safe slug
 * (`[a-zA-Z0-9_-]+`, enforced Rust-side by `is_valid_agent_slug`); a user
 * types a free-text display name ("My CTO!"), so the create-agent form needs
 * a client-side preview of the slug it will actually write to, matching the
 * Rust validator's rules exactly so the preview never lies about what will
 * be accepted.
 * What: Lowercases, replaces runs of whitespace/invalid characters with a
 * single `-`, and trims leading/trailing `-`. Returns `''` for input that
 * slugifies to nothing (e.g. all-punctuation) so callers can treat that as
 * "not yet a valid name" rather than silently writing an empty-string slug.
 * Test: `slugify_lowercases_and_hyphenates`, `slugify_strips_invalid_chars`,
 * `slugify_collapses_whitespace_runs`, `slugify_empty_input_returns_empty`.
 */
export function slugify(input: string): string {
  return input
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '');
}

/**
 * Why: The switcher and editor both need "catalog agents ∪ user overlays ∪
 * the always-present base assistant", deduplicated by id so an overlay that
 * happens to share a name with a catalog entry (or the base) doesn't render
 * twice. Centralizing the merge means both UIs stay in sync automatically.
 * What: Returns the base entry first, then catalog agents (skipping any
 * named `BASE_AGENT_ID`), then overlay agents (skipping any slug already
 * used by the base or a catalog entry) sorted by label. Catalog labels
 * prefer `display_name` (#3737), falling back to `name`. Overlay labels
 * prefer `display_name`, falling back to `name`, falling back to `slug`.
 * Test: `buildRoster_always_includes_base_first`,
 * `buildRoster_dedupes_catalog_entry_named_assistant`,
 * `buildRoster_dedupes_overlay_shadowing_catalog_entry`,
 * `buildRoster_sorts_overlays_by_label`,
 * `buildRoster_prefers_catalog_display_name`.
 */
export function buildRoster(
  catalog: CatalogAgent[],
  overlays: OverlayAgent[],
): RosterEntry[] {
  const seen = new Set<string>([BASE_AGENT_ID]);
  const entries: RosterEntry[] = [BASE_ENTRY];

  for (const agent of catalog) {
    if (seen.has(agent.name)) continue;
    seen.add(agent.name);
    entries.push({
      id: agent.name,
      // #3738: prefer the persona's display_name ("CTO Bot") over its stable
      // id ("cto-assistant") for the human-facing roster label. Canonical
      // single form shared with PR #3739 — `.trim() || name` so a blank or
      // whitespace-only display_name (the backend already falls back to name,
      // but a hand-edited overlay could slip one through) never yields an
      // empty label.
      label: agent.display_name?.trim() || agent.name,
      description: agent.description || undefined,
      source: 'catalog',
    });
  }

  const overlayEntries: RosterEntry[] = [];
  for (const overlay of overlays) {
    if (seen.has(overlay.slug)) continue;
    seen.add(overlay.slug);
    overlayEntries.push({
      id: overlay.slug,
      label: overlay.display_name || overlay.name || overlay.slug,
      source: 'overlay',
      extends: overlay.extends || undefined,
    });
  }
  overlayEntries.sort((a, b) => a.label.localeCompare(b.label));

  return [...entries, ...overlayEntries];
}

/**
 * Why (#3737, per-message chat attribution, epic #3052): each assistant chat
 * bubble must be labeled with WHICH persona produced it — the display name of
 * the agent active when the message was sent — never a generic "agent". The
 * dispatch axis is `activeAgentId` (see `stores/app.ts`): `null` means no
 * roster entry is explicitly selected, which routes through the tools-armed
 * base-assistant session path, so it labels as "Assistant". A non-null id is
 * looked up in the merged roster for its friendly `label`.
 * What: Returns the roster entry's `label` for `id`, or `BASE_AGENT_LABEL`
 * ("Assistant") when `id` is `null`/empty or no entry matches (e.g. a stale
 * selection, or the roster hasn't loaded yet). Pure — no store access — so
 * the caller stamps the name onto the message at creation time and a later
 * persona switch never retroactively relabels older bubbles.
 * Test: `rosterDisplayName_null_id_is_base_label`,
 * `rosterDisplayName_resolves_selected_entry`,
 * `rosterDisplayName_unknown_id_falls_back_to_base`.
 */
export function rosterDisplayName(
  roster: RosterEntry[],
  id: string | null,
): string {
  if (!id) return BASE_AGENT_LABEL;
  return roster.find((e) => e.id === id)?.label ?? BASE_AGENT_LABEL;
}

/**
 * Why (#3737): chat attribution must reflect who ANSWERED. When a turn
 * delegates (base "Assistant" → Izzie for a weather question), the server
 * reports the responder's agent NAME in `PmResponse.responder_agent`; the GUI
 * overwrites the bubble's request-time speaker stamp with that agent's display
 * name on `task-complete`. This differs from `rosterDisplayName`: a delegated
 * responder is always a real named specialist, so an id missing from the
 * roster (e.g. a bundled worker not enumerated by `/api/agents`' flat scan)
 * must fall back to the raw agent name — NOT to "Assistant", which would be a
 * fresh mis-attribution.
 * What: Returns the roster entry's `label` for `agentName`, else `agentName`
 * verbatim. Trims; returns `null` for a blank/empty input so callers can skip
 * the overwrite (keeping the request-time stamp).
 * Test: `responderDisplayName_resolves_known_agent`,
 * `responderDisplayName_falls_back_to_raw_name_for_unknown`,
 * `responderDisplayName_blank_returns_null`.
 */
export function responderDisplayName(
  roster: RosterEntry[],
  agentName: string | null | undefined,
): string | null {
  const name = agentName?.trim();
  if (!name) return null;
  return roster.find((e) => e.id === name)?.label ?? name;
}

/** Editable fields the create/edit form (#3224) collects for one overlay. */
export interface OverlayDraft {
  displayName: string;
  extends: string;
  instructions: string;
}

/**
 * Why: `write_personalization_overlay` writes whatever raw markdown string
 * it's given verbatim (#3061) — this function is the ONLY place that
 * assembles the flat-YAML-frontmatter + prose shape both the Rust loader
 * (`AgentConfig::by_name_async`, `extends:` resolution) and
 * `overlay.rs::parse_frontmatter_fields` expect, so the editor and the
 * backend agree on the format without duplicating the schema in two places.
 * What: Emits `---\nname: <slug>\n[extends: ...]\n[display_name: ...]\n---`
 * followed by a blank line and the trimmed instructions body. `extends` /
 * `displayName` lines are omitted when empty (matching the Rust parser's
 * "blank values are omitted" contract).
 * Test: `buildOverlayMarkdown_includes_all_fields`,
 * `buildOverlayMarkdown_omits_blank_optional_fields`.
 */
export function buildOverlayMarkdown(slug: string, draft: OverlayDraft): string {
  const lines = ['---', `name: ${slug}`];
  if (draft.extends.trim()) lines.push(`extends: ${draft.extends.trim()}`);
  if (draft.displayName.trim()) lines.push(`display_name: ${draft.displayName.trim()}`);
  lines.push('---', '', draft.instructions.trim(), '');
  return lines.join('\n');
}

/**
 * Why: Editing an existing overlay needs to split its raw markdown back into
 * the form's fields — inverse of `buildOverlayMarkdown`. Deliberately a
 * lenient line-scan (mirrors `overlay.rs::parse_frontmatter_fields`'s
 * "simple line scan, no YAML dependency" choice) rather than a full YAML
 * parser: every overlay this app writes has flat, single-line frontmatter,
 * so anything more only adds a dependency without expanding real coverage.
 * What: Returns `{ displayName: '', extends: '', instructions: content }`
 * unchanged when `content` doesn't open with a `---` line (nothing to
 * parse); otherwise reads `key: value` pairs until the closing `---` and
 * treats everything after it as `instructions`.
 * Test: `parseOverlayMarkdown_round_trips_buildOverlayMarkdown_output`,
 * `parseOverlayMarkdown_treats_content_without_frontmatter_as_instructions`.
 */
export function parseOverlayMarkdown(content: string): OverlayDraft {
  const lines = content.split('\n');
  if (lines[0]?.trim() !== '---') {
    return { displayName: '', extends: '', instructions: content.trim() };
  }
  const fm: Record<string, string> = {};
  let i = 1;
  for (; i < lines.length; i++) {
    if (lines[i].trim() === '---') {
      i++;
      break;
    }
    const idx = lines[i].indexOf(':');
    if (idx === -1) continue;
    const key = lines[i].slice(0, idx).trim();
    const value = lines[i]
      .slice(idx + 1)
      .trim()
      .replace(/^"|"$/g, '');
    if (value) fm[key] = value;
  }
  return {
    displayName: fm.display_name ?? '',
    extends: fm.extends ?? '',
    instructions: lines.slice(i).join('\n').trim(),
  };
}
