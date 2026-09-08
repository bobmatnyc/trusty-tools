// Regression tests for the console link-back address rule (#6439).
//
// Why: owner ruling 2026-08-31 — a relative link when served through the
// console, the well-known default `http://127.0.0.1:7788/` standalone, no
// config knob. These tests pin both branches against the console's real
// mount path (`crates/trusty-console/src/tools_ui.rs`: `/tools/search/`).
// What: drives `resolveConsoleUrl` with explicit pathnames — no DOM stubbing
// needed, unlike base.test.js, since the function takes the pathname as an
// argument.
// Test: this file — `pnpm test`.

import { describe, expect, it } from 'vitest';

import { CONSOLE_DEFAULT_URL, resolveConsoleUrl } from './consoleLink.js';

describe('resolveConsoleUrl — served through the console', () => {
  it('returns a relative link at the mount root', () => {
    expect(resolveConsoleUrl('/tools/search/')).toBe('/');
  });

  it('returns a relative link at a sub-path of the mount', () => {
    expect(resolveConsoleUrl('/tools/search/indexes')).toBe('/');
  });

  it('is agnostic to which console-mounted tool is asking', () => {
    // The prefix check is generic — this dashboard cares only that it is
    // reachable under /tools/, not which sibling tool that path names.
    expect(resolveConsoleUrl('/tools/memory/')).toBe('/');
  });
});

describe('resolveConsoleUrl — served standalone by its own daemon', () => {
  it('falls back to the well-known default at the bare root', () => {
    expect(resolveConsoleUrl('/')).toBe(CONSOLE_DEFAULT_URL);
  });

  it('falls back to the well-known default under the SPA hash router', () => {
    expect(resolveConsoleUrl('/index.html')).toBe(CONSOLE_DEFAULT_URL);
  });

  it('does not match a path that merely contains "tools" mid-segment', () => {
    expect(resolveConsoleUrl('/some-tools-page/')).toBe(CONSOLE_DEFAULT_URL);
  });
});

describe('resolveConsoleUrl — default argument', () => {
  it('reads window.location.pathname when no argument is given', () => {
    // jsdom's default test location is http://localhost/.
    expect(resolveConsoleUrl()).toBe(CONSOLE_DEFAULT_URL);
  });
});
