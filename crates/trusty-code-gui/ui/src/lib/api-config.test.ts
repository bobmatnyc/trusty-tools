// Why: `apiBase()` decides where every fetch() in this app goes. Since #10166
// the base is the `tcode-bridge://` custom scheme the Rust side answers
// in-process, and browser mode is gone (owner ruling Q2). The risks are the
// same as before: a base invented in web mode would send this app's requests,
// credential attached, to whatever answers there, and a Tauri base that is
// not what the Rust side named would reach nothing.
// What: asserts Tauri mode asks the Rust side and web mode invents nothing.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { apiBase, isTauri } from './api-config';
import { enterTauri, fakeTauri, invoke, leaveTauri } from './tauri-fake';

vi.mock('@tauri-apps/api/core', () => import('./tauri-fake'));

afterEach(() => {
  leaveTauri();
  fakeTauri.reset();
  localStorage.clear();
});

describe('apiBase', () => {
  it('in Tauri mode, is the scheme origin the Rust side names', async () => {
    enterTauri();
    expect(isTauri()).toBe(true);
    await expect(apiBase()).resolves.toBe('tcode-bridge://localhost');
    expect(invoke).toHaveBeenCalledWith('get_daemon_url');
  });

  it('in a plain browser tab, invents no URL', async () => {
    expect(isTauri()).toBe(false);
    await expect(apiBase()).resolves.toBe('');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('ignores the retired localStorage override (owner ruling Q2)', async () => {
    // The key `pnpm dev` used to point a browser tab at the loopback port.
    localStorage.setItem('trusty-code.daemonUrl', 'http://127.0.0.1:54321');
    await expect(apiBase()).resolves.toBe('');
  });
});
