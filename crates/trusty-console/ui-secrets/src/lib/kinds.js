/*
 * Why: #9067 — a refusal must never render blank. The console relays a
 * server refusal as `{"error": <fixed text>, "kind": <kind>}` only for the 37
 * kinds in its closed table (crates/trusty-console/src/secrets_uds/errors.rs
 * `UPSTREAM_KINDS`); its own refusals carry `error` and no `kind`.
 * What: `KIND_TEXT`, one sentence per upstream kind; `AGENT_HINT`, the one
 * extra line an `agent_use_refused` gets (Architect P4); `errorLines(err)`,
 * the lines to show for an error, never empty.
 * Test: `src/lib/kinds.test.js`; `page_kind_texts_cover_every_upstream_kind`
 * in secrets_uds/tests.rs pins the key set to `UPSTREAM_KINDS`.
 */

export const KIND_TEXT = {
  invalid_params: 'The secrets server refused the request as malformed.',
  project_invalid: 'The project is not a usable checkout.',
  project_unresolved: 'The secrets server could not work out the scope of this project.',
  vault_out_of_scope: 'That scope is not one this project may use.',
  invalid_value: 'The value is not one the secrets server can store.',
  not_found: 'The secret was not found.',
  unsupported: 'The backend does not support this operation.',
  unknown_backend: 'The secrets server does not know that backend.',
  backend_failed: 'The backend failed.',
  orphaned_backend_entry: 'The backend holds an entry the index does not list.',
  index_corrupt: 'The secrets index is damaged.',
  index_busy: 'The secrets store is busy. Try again.',
  storage_unavailable: 'The secrets store is not available.',
  config_invalid: 'The secrets configuration is not valid.',
  home_unavailable: 'The secrets server cannot find its home directory.',
  same_backend: 'Copy needs two different backends.',
  agent_use_refused: 'This key is denied to agents, or the operation is operator-only.',
  invalid_env_entry: 'An environment entry is not valid.',
  env_resolution_failed: 'An environment entry could not be resolved.',
  dotenv_syntax: 'A .env file has a syntax error.',
  remote_host_unsupported: "The project's remote host is not supported.",
  storage_refused: 'The secrets server refused the storage location.',
  tracked_backend_refused: "The project's tracked config selects a backend only the machine may select.",
  audit_unavailable: 'The audit log cannot be written, so the request was refused.',
  tracked_audit_refused: "The project's tracked config changes an audit setting only the machine may change.",
  tracked_cli_setting_refused: "The project's tracked config sets a CLI setting only the machine may set.",
  cli_not_installed: "The backend's command-line tool is not installed.",
  backend_locked: 'The backend is locked or signed out.',
  file_backend_not_selected: 'The file backend is not selected on this machine.',
  backend_not_enabled: 'The backend is not enabled on this machine.',
  deadline_exceeded: 'The secrets server ran out of time.',
  vault_not_visible: 'That scope is not visible to this caller.',
  backend_timeout: 'The backend did not answer in time.',
  grant_refused: 'The secrets server refused the grant.',
  grant_limit_reached: 'The grant limit was reached.',
  bridge_refused: 'The secrets server refused the console bridge.',
  internal: 'The secrets server failed.',
};

/** The hint an `agent_use_refused` adds (Architect P4): no pid, no path. */
export const AGENT_HINT =
  'The console could not map the caller to a person and treated it as an agent.';

/** Text for an HTTP status when a response carried no usable `error`. */
function statusText(status) {
  if (!status) return 'The console could not be reached.';
  return `The console refused the request (HTTP ${status}).`;
}

/**
 * The lines to show for a failed call. Never empty, never blank.
 * @param {{status?: number, kind?: string, error?: string}} err
 * @returns {string[]}
 */
export function errorLines(err) {
  const kind = typeof err?.kind === 'string' ? err.kind : null;
  if (kind && Object.hasOwn(KIND_TEXT, kind)) {
    return kind === 'agent_use_refused' ? [KIND_TEXT[kind], AGENT_HINT] : [KIND_TEXT[kind]];
  }
  const fixed = typeof err?.error === 'string' && err.error.trim() !== '' ? err.error : null;
  if (fixed) return [kind ? `${fixed} (${kind})` : fixed];
  return [statusText(err?.status)];
}
