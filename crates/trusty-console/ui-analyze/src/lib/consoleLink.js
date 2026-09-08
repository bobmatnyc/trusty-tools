// VENDORED COPY (#6439) — canonical source:
//   docs/design/UI/design-system-svelte/src/lib/consoleLink.js
// Sibling vendored copies:
//   - crates/trusty-console/ui-search/src/lib/consoleLink.js
//   - crates/trusty-console/ui-memory/src/lib/consoleLink.js
// Until #3492 (`@trusty/foundry` package) ships a real import, distribution
// is copy-paste, same as docs/design/UI/design-system/icons/ActionIcon.svelte
// (see that file's header and ../design-system/icons/README.md for the
// pattern). Fix a bug or extend the detection in the canonical file first,
// then propagate to every vendored copy in the same PR.
//
// Why: #6439 (owner directive 2026-09-08) — every service dashboard the
// console can serve (trusty-search, trusty-memory, trusty-analyze) shows a
// link back to the console. The owner's ruling on 2026-08-31 fixed the
// address rule: a same-origin relative link when the dashboard is served
// THROUGH the console, and the well-known default `http://127.0.0.1:7788/`
// as the standalone fallback — no config knob.
//
// What: `resolveConsoleUrl` tells the two cases apart by the URL path the
// page was loaded at. `crates/trusty-console/src/tools_ui.rs` mounts every
// tool dashboard it embeds under `/tools/<tool>/` (`GET /tools/search/`,
// `/tools/memory/`, `/tools/analyze/`, each with a `{*path}` catch-all
// beneath it) — a path under that prefix can only be reached by the console
// itself, so the console's own origin is address enough: an absolute path,
// `/`, resolves there regardless of the tool's own sub-path depth. Anywhere
// else, this dashboard is being served by its own daemon standalone, and the
// console is not reachable via the current origin — the well-known default
// is the only fallback the owner's ruling allows.
//
// Test: `consoleLink.test.js` in each vendored location covers both branches
// plus the pathname argument's default (`window.location.pathname`).

/** The console's well-known standalone default (owner ruling 2026-08-31). */
export const CONSOLE_DEFAULT_URL = 'http://127.0.0.1:7788/';

/**
 * The path prefix every console-mounted tool dashboard is served under.
 * See `crates/trusty-console/src/tools_ui.rs`: `GET /tools/<tool>/{*path}`.
 */
const CONSOLE_MOUNT_PREFIX = '/tools/';

/**
 * Resolve the console's address for a "back to the console" link.
 *
 * @param {string} [pathname] The path to test — defaults to
 *   `window.location.pathname` in a browser, and `''` (never console-served)
 *   outside one, so this stays callable from a non-browser test module.
 * @returns {string} `'/'` when served through the console, else
 *   {@link CONSOLE_DEFAULT_URL}.
 */
export function resolveConsoleUrl(
  pathname = typeof window !== 'undefined' ? window.location.pathname : ''
) {
  return pathname.startsWith(CONSOLE_MOUNT_PREFIX) ? '/' : CONSOLE_DEFAULT_URL;
}
