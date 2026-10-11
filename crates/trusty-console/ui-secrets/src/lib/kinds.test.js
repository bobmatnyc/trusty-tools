import { describe, expect, test } from 'vitest';
import { AGENT_HINT, KIND_TEXT, errorLines } from './kinds.js';

// The console's closed table (secrets_uds/errors.rs UPSTREAM_KINDS). The Rust
// test `page_kind_texts_cover_every_upstream_kind` pins KIND_TEXT's keys to it.
const UPSTREAM_KINDS = [
  'invalid_params', 'project_invalid', 'project_unresolved', 'vault_out_of_scope',
  'invalid_value', 'not_found', 'unsupported', 'unknown_backend', 'backend_failed',
  'orphaned_backend_entry', 'index_corrupt', 'index_busy', 'storage_unavailable',
  'config_invalid', 'home_unavailable', 'same_backend', 'agent_use_refused',
  'invalid_env_entry', 'env_resolution_failed', 'dotenv_syntax', 'remote_host_unsupported',
  'storage_refused', 'tracked_backend_refused', 'audit_unavailable', 'tracked_audit_refused',
  'tracked_cli_setting_refused', 'cli_not_installed', 'backend_locked',
  'file_backend_not_selected', 'backend_not_enabled', 'deadline_exceeded',
  'vault_not_visible', 'backend_timeout', 'grant_refused', 'grant_limit_reached',
  'bridge_refused', 'internal',
];

const nonBlank = (lines) => lines.length > 0 && lines.every((l) => l.trim() !== '');

describe('errorLines', () => {
  test('every upstream kind renders its own text', () => {
    expect(UPSTREAM_KINDS).toHaveLength(37);
    expect(Object.keys(KIND_TEXT).sort()).toEqual([...UPSTREAM_KINDS].sort());
    for (const kind of UPSTREAM_KINDS) {
      const lines = errorLines({ status: 403, error: 'fixed', kind });
      expect(nonBlank(lines), kind).toBe(true);
      expect(lines[0]).toBe(KIND_TEXT[kind]);
    }
  });

  test('an unknown kind renders the console text', () => {
    const lines = errorLines({ status: 502, error: 'the secrets server answered unexpectedly', kind: 'brand_new_kind' });
    expect(nonBlank(lines)).toBe(true);
    expect(lines[0]).toContain('the secrets server answered unexpectedly');
  });

  test('an unknown kind with no text still renders text', () => {
    expect(nonBlank(errorLines({ status: 500, kind: 'brand_new_kind' }))).toBe(true);
  });

  test('a console refusal with no kind renders its fixed text', () => {
    const lines = errorLines({ status: 403, error: "the request is not from this console's own page" });
    expect(lines).toEqual(["the request is not from this console's own page"]);
  });

  test('a refusal with no body renders a status line', () => {
    expect(nonBlank(errorLines({ status: 503 }))).toBe(true);
    expect(nonBlank(errorLines({ status: 0 }))).toBe(true);
    expect(nonBlank(errorLines({ status: 403, error: '   ' }))).toBe(true);
  });

  // #9067 P4: the fixed text plus one hint line, naming no pid or path.
  test('agent_use_refused adds the one hint line', () => {
    const lines = errorLines({ status: 403, kind: 'agent_use_refused' });
    expect(lines).toEqual([KIND_TEXT.agent_use_refused, AGENT_HINT]);
    expect(AGENT_HINT).not.toMatch(/\d|\//);
    expect(AGENT_HINT).not.toMatch(/pid/i);
  });
});
