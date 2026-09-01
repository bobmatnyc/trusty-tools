/**
 * The running console's version, as shown in the header lockup descriptor.
 *
 * Why: the operator asked the header to name the version it is actually
 * running. The SPA bundle is committed to the repo and rebuilt only when the UI
 * changes, so a build-time constant compiled into the JS would keep reporting
 * whatever version was current when the bundle was last regenerated. The
 * version therefore comes from the server at runtime — `GET /health` already
 * answers with this binary's `CARGO_PKG_VERSION`, so no new route exists for
 * this.
 *
 * What: two pure functions plus one fetch wrapper that never rejects. An
 * unknown version is `null` everywhere, and `describeConsole(null)` returns the
 * descriptor exactly as it read before this feature — a failed or still-pending
 * probe leaves the lockup unchanged rather than showing a gap or "undefined".
 * Test: `consoleVersion.test.js` — run `node --test src/consoleVersion.test.js`
 * from `crates/trusty-console/ui`.
 */

/**
 * The Foundry descriptor line, uppercase IBM Plex Mono with a `·` separator.
 *
 * Kept here rather than inline in `BrandLockup.svelte` so the version suffix is
 * appended in one tested place, using the same separator as the descriptor's
 * own two halves (`docs/design/UI/icons/README.md`).
 */
export const CONSOLE_DESCRIPTOR = 'UNIT-05 · SERVICE CONSOLE';

/**
 * Read a version string out of an arbitrary `/health` payload.
 *
 * Anything that is not a non-empty string is "no version": a daemon that
 * predates the field sends nothing, and a malformed body could send a number or
 * `null`. All of them must reach the descriptor as `null`, because the one
 * outcome this module exists to prevent is rendering `undefined` into the
 * brand lockup.
 */
export function versionFrom(payload) {
  const value = payload?.version;
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  return trimmed === '' ? null : trimmed;
}

/**
 * The descriptor text for a known or unknown version.
 *
 * A known version is appended as `· v<version>`; an unknown one leaves the
 * descriptor byte-for-byte as it was.
 */
export function describeConsole(version) {
  return version ? `${CONSOLE_DESCRIPTOR} · v${version}` : CONSOLE_DESCRIPTOR;
}

/**
 * Ask the server which version it is running, resolving to `null` on any
 * failure.
 *
 * The header must render whether or not this probe succeeds, so a transport
 * error, a non-2xx status, and an unparseable body are all the same answer as a
 * missing field. `fetchImpl` is a parameter so the failure paths are testable
 * without a network.
 */
export async function fetchConsoleVersion(fetchImpl = fetch) {
  try {
    const resp = await fetchImpl('/health');
    if (!resp.ok) return null;
    return versionFrom(await resp.json());
  } catch {
    return null;
  }
}
