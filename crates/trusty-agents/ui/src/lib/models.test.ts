// Unit tests for the pure model/provider picker helpers (#3245, epic #3052).
// See `models.ts` for the Why/What of each function under test.

import { describe, expect, it } from 'vitest';
import {
  DEFAULT_PICKER_ID,
  buildPicker,
  resolveOverride,
  type ModelsCatalogResponse,
} from './models';

const CATALOG: ModelsCatalogResponse = {
  providers: [
    {
      provider_id: 'openrouter',
      default_model: 'anthropic/claude-sonnet-4-6',
      context_window: 200000,
      credential_configured: true,
      reachable_today: true,
    },
    {
      provider_id: 'anthropic',
      default_model: 'claude-sonnet-4-6',
      context_window: 200000,
      credential_configured: true,
      reachable_today: true,
    },
    {
      provider_id: 'bedrock',
      default_model: 'us.anthropic.claude-sonnet-4-6',
      context_window: 200000,
      credential_configured: true,
      reachable_today: true,
    },
    {
      provider_id: 'openai',
      default_model: 'gpt-4o',
      context_window: 128000,
      credential_configured: false,
      reachable_today: false,
    },
  ],
  local: {
    provider_id: 'ollama',
    default_model: 'llama3.2',
    available: true,
    reachable_today: true,
  },
};

describe('buildPicker', () => {
  it('always includes the default entry first', () => {
    const picker = buildPicker(CATALOG);
    expect(picker[0].id).toBe(DEFAULT_PICKER_ID);
    expect(picker[0].selectable).toBe(true);
  });

  it('marks uncredentialed providers unselectable', () => {
    const picker = buildPicker(CATALOG);
    const openai = picker.find((e) => e.id === 'openai');
    expect(openai?.selectable).toBe(false);
  });

  it('marks credentialed + reachable providers selectable', () => {
    const picker = buildPicker(CATALOG);
    const openrouter = picker.find((e) => e.id === 'openrouter');
    expect(openrouter?.selectable).toBe(true);
    expect(openrouter?.modelId).toBe('anthropic/claude-sonnet-4-6');
  });

  it('appends the local entry with a normalized "local" provider id, not "ollama"', () => {
    const picker = buildPicker(CATALOG);
    const local = picker[picker.length - 1];
    expect(local.id).toBe('local');
    expect(local.providerId).toBe('local');
    expect(local.modelId).toBe('llama3.2');
    expect(local.selectable).toBe(true);
  });

  it('marks the local entry unselectable when Ollama is unavailable', () => {
    const offline: ModelsCatalogResponse = {
      ...CATALOG,
      local: { ...CATALOG.local, available: false },
    };
    const picker = buildPicker(offline);
    expect(picker[picker.length - 1].selectable).toBe(false);
  });
});

describe('resolveOverride', () => {
  it('omits both fields for the default entry', () => {
    const picker = buildPicker(CATALOG);
    const result = resolveOverride(picker[0]);
    expect(result).toEqual({ modelId: null, providerId: null });
  });

  it('omits both fields for an unselectable entry', () => {
    const picker = buildPicker(CATALOG);
    const openai = picker.find((e) => e.id === 'openai')!;
    const result = resolveOverride(openai);
    expect(result).toEqual({ modelId: null, providerId: null });
  });

  it('passes through provider_id for openrouter and bedrock', () => {
    const picker = buildPicker(CATALOG);
    const openrouter = picker.find((e) => e.id === 'openrouter')!;
    expect(resolveOverride(openrouter)).toEqual({
      modelId: 'anthropic/claude-sonnet-4-6',
      providerId: 'openrouter',
    });
    const bedrock = picker.find((e) => e.id === 'bedrock')!;
    expect(resolveOverride(bedrock)).toEqual({
      modelId: 'us.anthropic.claude-sonnet-4-6',
      providerId: 'bedrock',
    });
  });

  it('passes through provider_id "local" for the Ollama entry', () => {
    const picker = buildPicker(CATALOG);
    const local = picker[picker.length - 1];
    expect(resolveOverride(local)).toEqual({
      modelId: 'llama3.2',
      providerId: 'local',
    });
  });

  it('sends model_id only (omits provider_id) for a provider outside the accepted override set', () => {
    const picker = buildPicker(CATALOG);
    const anthropic = picker.find((e) => e.id === 'anthropic')!;
    expect(resolveOverride(anthropic)).toEqual({
      modelId: 'claude-sonnet-4-6',
      providerId: null,
    });
  });
});
