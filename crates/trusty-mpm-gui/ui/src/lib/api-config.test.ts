// Why: #3315 deleted `apiBase()`'s `trusty-mpm.daemonUrl` localStorage
// override — nothing ever wrote that key, and even if it had been set the
// CSP `connect-src` in `tauri.conf.json` (pinned to DEFAULT_DAEMON_URL) would
// silently block the resulting request, so the override could never work.
// `apiBase()` now has a single behavior: always resolve to
// `DEFAULT_DAEMON_URL`. It no longer reads `window`/`localStorage` at all, so
// these tests run under a plain Node environment (see `vitest.config.ts`) —
// no DOM is needed to exercise it.
// What: Asserts `apiBase()`'s constant return value, pins `DEFAULT_DAEMON_URL`
// against the independently-maintained Rust constant in
// `trusty-mpm-gui/src/state.rs` so the two copies can't silently drift, and
// confirms `isTauri()` (unchanged by this fix) still behaves outside a Tauri
// runtime.
// Test: this file.
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { apiBase, DEFAULT_DAEMON_URL, isTauri } from './api-config';

const __dirname = dirname(fileURLToPath(import.meta.url));
// ui root is crates/trusty-mpm-gui/ui; the Rust constant lives in the
// sibling `src/` tree of the same crate.
const stateRs = resolve(__dirname, '../../../src/state.rs');

describe('apiBase', () => {
  it('always resolves to DEFAULT_DAEMON_URL — the only remaining behavior post-#3315', () => {
    expect(apiBase()).toBe(DEFAULT_DAEMON_URL);
    // Calling it repeatedly must not depend on any hidden mutable state
    // (there is none left to depend on — this pins that).
    expect(apiBase()).toBe(DEFAULT_DAEMON_URL);
  });
});

describe('DEFAULT_DAEMON_URL', () => {
  it('matches the Rust-side GuiState::DEFAULT_DAEMON_URL', () => {
    const rust = readFileSync(stateRs, 'utf-8');
    const match = rust.match(/pub const DEFAULT_DAEMON_URL: &str = "([^"]+)";/);
    expect(match, `could not find DEFAULT_DAEMON_URL constant in ${stateRs}`).not.toBeNull();
    expect(DEFAULT_DAEMON_URL).toBe(match?.[1]);
  });
});

describe('isTauri', () => {
  it('is false outside a Tauri/browser runtime (unchanged by #3315)', () => {
    expect(isTauri()).toBe(false);
  });
});
