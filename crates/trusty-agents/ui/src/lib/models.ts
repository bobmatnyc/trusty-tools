// Model/provider picker helpers (#3245, epic #3052).
//
// Why: `GET /api/models` (#3243) returns the live inference-provider catalog
// (see `crates/trusty-agents/src/api/server/models.rs`); the picker needs to
// turn that catalog into a flat, selectable list and turn a selected row
// into the `model_id`/`provider_id` pair `POST /api/task` accepts. Both are
// pure functions (no fetch/invoke) so they're unit-testable without a
// running API server, mirroring `lib/roster.ts`'s split between pure
// helpers here and data fetching in `stores/app.ts`.
// What: `ModelProviderEntry`/`LocalModelEntry`/`ModelsCatalogResponse` types
// (mirroring the Rust response shape verbatim), `PickerEntry`, `buildPicker`,
// and `resolveOverride`.
// Test: `models.test.ts`.

/** One row in `GET /api/models`'s `providers` array. */
export interface ModelProviderEntry {
  provider_id: string;
  default_model: string;
  context_window: number;
  credential_configured: boolean;
  reachable_today: boolean;
}

/** The synthetic local-Ollama entry in `GET /api/models`'s `local` field. */
export interface LocalModelEntry {
  provider_id: string;
  default_model: string;
  available: boolean;
  reachable_today: boolean;
}

/** Full `GET /api/models` response body. */
export interface ModelsCatalogResponse {
  providers: ModelProviderEntry[];
  local: LocalModelEntry;
}

/**
 * Why: The picker's "Default" row must always be selectable and must be
 * distinguishable from every catalog id in `POST /api/task`'s `model_id`/
 * `provider_id` fields, since selecting it is what makes InputArea OMIT both
 * fields (see `resolveOverride`'s doc comment) and fall back to the current
 * pre-#3245 behavior byte-for-byte.
 */
export const DEFAULT_PICKER_ID = '__default__';

/** One row the `ModelSwitcher` dropdown renders. */
export interface PickerEntry {
  /** Stable id for the row — `DEFAULT_PICKER_ID` or a provider id. */
  id: string;
  label: string;
  /** `false` disables the row (no credential, or not yet wired for dispatch). */
  selectable: boolean;
  modelId: string | null;
  providerId: string | null;
}

/**
 * Why: `resolve_overridden_credentials` (Rust, `ctrl/pm_task/helpers.rs`)
 * only accepts `"claude-code" | "openrouter" | "bedrock" | "local"` as a
 * `provider_id` override — sending any other registry provider id (e.g.
 * `"anthropic"`, `"openai"`) would fail the turn with a clear "unknown
 * provider override" error rather than silently falling back. The registry
 * catalog legitimately lists more providers than that (extension providers
 * like Fireworks/Together/AtlasCloud, plus Anthropic direct), so only a
 * subset of catalog rows may safely carry BOTH fields.
 * What: The provider ids for which it's safe to send `provider_id` alongside
 * `model_id`. Every other catalog row still gets a picker entry (so the
 * model choice itself is visible and can be pinned via `model_id` alone,
 * letting normal env-credential resolution pick the transport) but
 * `resolveOverride` omits `providerId` for it — see that function.
 */
const ACCEPTED_PROVIDER_OVERRIDES = new Set(['openrouter', 'bedrock']);

/**
 * Why: The switcher needs one flat list — a "Default" row plus one row per
 * catalog provider plus the synthetic Ollama row — sorted the same way every
 * render so the dropdown doesn't jitter.
 * What: Prepends a `DEFAULT_PICKER_ID` row (always selectable), maps
 * `providers` to rows (`selectable` = `credential_configured && reachable_today`),
 * then appends the local/Ollama row (`selectable` = `local.available`) with
 * `providerId` normalized to `"local"` — the value `resolve_overridden_credentials`
 * actually accepts — rather than the catalog's own `"ollama"` id.
 * Test: `buildPicker_always_includes_default_first`,
 * `buildPicker_marks_uncredentialed_providers_unselectable`,
 * `buildPicker_marks_unreachable_providers_unselectable`,
 * `buildPicker_appends_local_entry_with_normalized_provider_id`.
 */
export function buildPicker(catalog: ModelsCatalogResponse): PickerEntry[] {
  const defaultEntry: PickerEntry = {
    id: DEFAULT_PICKER_ID,
    label: 'Default',
    selectable: true,
    modelId: null,
    providerId: null,
  };

  const providerEntries: PickerEntry[] = catalog.providers.map((p) => ({
    id: p.provider_id,
    label: `${p.provider_id} — ${p.default_model}`,
    selectable: p.credential_configured && p.reachable_today,
    modelId: p.default_model,
    providerId: p.provider_id,
  }));

  const localEntry: PickerEntry = {
    id: 'local',
    label: `local — ${catalog.local.default_model}`,
    selectable: catalog.local.available,
    modelId: catalog.local.default_model,
    providerId: 'local',
  };

  return [defaultEntry, ...providerEntries, localEntry];
}

/** The `model_id`/`provider_id` pair to send in `POST /api/task`. */
export interface OverridePayload {
  modelId: string | null;
  providerId: string | null;
}

/**
 * Why: `InputArea.svelte` must send exactly what the backend can act on —
 * never a `provider_id` value `resolve_overridden_credentials` will reject.
 * Centralizing the mapping here (rather than inline in the component) keeps
 * it unit-testable and gives `ModelSwitcher`/`InputArea` one shared source of
 * truth for "what does picking this row actually submit."
 * What: `DEFAULT_PICKER_ID` and non-selectable entries resolve to
 * `{ modelId: null, providerId: null }` (no override sent — the request
 * omits both fields and behaves exactly as before #3245). Otherwise returns
 * `entry.modelId` verbatim, and `entry.providerId` only when it's in
 * `ACCEPTED_PROVIDER_OVERRIDES` or is the normalized `"local"` id — every
 * other provider id (anthropic, openai, together, atlascloud, fireworks)
 * still pins the model via `modelId` but omits `providerId`, letting the
 * normal env-credential probe pick the transport.
 * Test: `resolveOverride_default_entry_omits_both_fields`,
 * `resolveOverride_unselectable_entry_omits_both_fields`,
 * `resolveOverride_openrouter_and_bedrock_pass_through_provider_id`,
 * `resolveOverride_local_passes_through_provider_id`,
 * `resolveOverride_unaccepted_provider_sends_model_id_only`.
 */
export function resolveOverride(entry: PickerEntry): OverridePayload {
  if (entry.id === DEFAULT_PICKER_ID || !entry.selectable) {
    return { modelId: null, providerId: null };
  }
  const providerId =
    entry.providerId &&
    (entry.providerId === 'local' || ACCEPTED_PROVIDER_OVERRIDES.has(entry.providerId))
      ? entry.providerId
      : null;
  return { modelId: entry.modelId, providerId };
}
