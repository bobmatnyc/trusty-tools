/*
 * Why: Centralised reactive state for daemon health + status so the topbar
 * and multiple views share one source of truth without each refetching.
 * What: Svelte 5 rune-backed getters plus refresh helpers. Shapes are flat so
 * views can `$derived(getX())` directly.
 * Test: `src/lib/state.test.js`.
 */

import { api } from './api.js';

let _health = $state(null);
let _status = $state(null);
let _healthError = $state(null);
let _statusError = $state(null);

export function getHealth() {
  return _health;
}

export function getStatus() {
  return _status;
}

/**
 * Why (#6155): this reads as "is the daemon reachable", and `refreshStatus`
 * used to write into the SAME slot. A slow or failing `/api/v1/status` — a
 * different call, against a daemon that is answering `/health` in 25 ms — then
 * presented as a connection failure. The two are separate signals now.
 * What: the last `/health` failure, or `null`.
 */
export function getError() {
  return _healthError;
}

/** The last `/api/v1/status` failure, or `null`. Never a reachability claim. */
export function getStatusError() {
  return _statusError;
}

/**
 * Why (#6155): the version badge flips to `offline` off this snapshot, so only
 * `/health` itself may set it. Nothing else in this module writes `_health`.
 * What: replaces the snapshot with the daemon's, or with `unreachable` when the
 * `/health` call — and only that call — failed.
 * Test: `src/lib/state.test.js` — `a failing status leaves health alone`.
 */
export async function refreshHealth() {
  try {
    _health = await api.health();
    _healthError = null;
  } catch (e) {
    _health = { status: 'unreachable', version: '' };
    _healthError = e.message || String(e);
  }
  return _health;
}

/** Refresh the aggregate counts. A failure here is not a reachability claim. */
export async function refreshStatus() {
  try {
    _status = await api.status();
    _statusError = null;
  } catch (e) {
    _statusError = e.message || String(e);
  }
  return _status;
}
