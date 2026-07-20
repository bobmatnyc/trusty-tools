// Why: `DEFAULT_DAEMON_URL` is a THIRD manually-maintained copy of tcode's
// default HTTP port — alongside `trusty_code::serve::DEFAULT_HTTP_PORT`
// (the source of truth) and `trusty-code-gui/src/state.rs::DEFAULT_DAEMON_URL`
// (the Tauri IPC fallback, pinned to the Rust constant by
// `default_daemon_url_matches_tcode_default_http_port`). This is the WEB-MODE
// fallback (`apiBase()`'s non-Tauri branch — the default.md `pnpm dev` loop,
// and any browser tab that isn't the Tauri webview): #3364 shipped a fix that
// updated the Tauri path and its pinning test but missed this literal
// entirely, so web mode kept binding the old (colliding) port. Nothing at
// compile/build time ties a `.ts` literal to a Rust `pub const`, so this test
// parses the real Rust source and asserts the two values agree, the same way
// the Rust-side `known_siblings` guard tests assert port uniqueness by
// re-reading pointer-commented values rather than trusting them silently.
// What: reads `crates/trusty-code/src/serve/mod.rs`, extracts
// `DEFAULT_HTTP_PORT`'s numeric value via regex, and asserts
// `DEFAULT_DAEMON_URL` embeds that exact port. Also pins the literal value
// directly as a cheap, redundant belt-and-suspenders check.
// Test: this file.
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { DEFAULT_DAEMON_URL } from './api-config';

const __dirname = dirname(fileURLToPath(import.meta.url));
// ui root is crates/trusty-code-gui/ui; the daemon crate is a sibling of
// trusty-code-gui under crates/.
const serveModRs = resolve(
  __dirname,
  '../../../../trusty-code/src/serve/mod.rs',
);

describe('DEFAULT_DAEMON_URL', () => {
  it('pins the documented default (7882) so a future edit is deliberate', () => {
    expect(DEFAULT_DAEMON_URL).toBe('http://127.0.0.1:7882');
  });

  it('embeds the exact port trusty_code::serve::DEFAULT_HTTP_PORT compiles to', () => {
    const rust = readFileSync(serveModRs, 'utf-8');
    const match = rust.match(/pub const DEFAULT_HTTP_PORT: u16 = (\d+);/);
    expect(
      match,
      `could not find "pub const DEFAULT_HTTP_PORT: u16 = <N>;" in ${serveModRs}`,
    ).not.toBeNull();
    const rustPort = match?.[1];
    expect(DEFAULT_DAEMON_URL).toBe(`http://127.0.0.1:${rustPort}`);
  });
});
