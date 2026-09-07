/*
 * Why: Centralised reactive state for daemon health + status so the topbar
 * and multiple views share one source of truth without each refetching.
 * What: Svelte 5 rune-backed getters plus refresh helpers. Shapes are flat so
 * views can `$derived(getX())` directly.
 * Test: call refreshHealth(), assert getHealth() reflects the /health payload.
 */

import { api } from './api.js';

let _health = $state(null);
let _status = $state(null);
let _error = $state(null);

export function getHealth() {
  return _health;
}

export function getStatus() {
  return _status;
}

export function getError() {
  return _error;
}

export async function refreshHealth() {
  try {
    _health = await api.health();
    _error = null;
  } catch (e) {
    _health = { status: 'unreachable', version: '' };
    _error = e.message || String(e);
  }
  return _health;
}

export async function refreshStatus() {
  try {
    _status = await api.status();
  } catch (e) {
    _error = e.message || String(e);
  }
  return _status;
}
