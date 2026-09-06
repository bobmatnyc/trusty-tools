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
 * Read the console's uptime in whole seconds out of a `/health` payload
 * (#6908).
 *
 * Why `null` rather than `0` for anything unreadable: the console's details
 * pane renders a dash for an unknown uptime and a duration for a known one, and
 * a daemon that predates the field would otherwise claim it just started. A
 * negative number is rejected for the same reason — it is not a duration.
 * Test: `uptimeFrom reads whole seconds`, `uptimeFrom rejects a non-duration`.
 */
export function uptimeFrom(payload) {
  const value = payload?.uptime_secs;
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return null;
  return Math.floor(value);
}

/**
 * Ask the server what it is running and how long it has been running,
 * resolving to nulls on any failure.
 *
 * Why one probe for both: the console's details pane wants the version and the
 * uptime together, and `/health` answers both in one body. Splitting them would
 * be two reads of the same route whose answers could name different builds
 * across a restart. `fetchConsoleVersion` below is the version-only caller —
 * `BrandLockup` mounts under the screensaver too, where no page owner has a
 * health read to hand it.
 * What: `{ version, uptimeSecs }`, either half `null` when the field is absent
 * or unreadable. Never rejects — a transport error, a non-2xx status and an
 * unparseable body are all the same answer as a missing field.
 * Test: `fetchConsoleHealth reads both fields`, `fetchConsoleHealth reports
 * nulls on a failed probe`.
 */
export async function fetchConsoleHealth(fetchImpl = fetch) {
  try {
    const resp = await fetchImpl('/health');
    if (!resp.ok) return { version: null, uptimeSecs: null };
    const payload = await resp.json();
    return { version: versionFrom(payload), uptimeSecs: uptimeFrom(payload) };
  } catch {
    return { version: null, uptimeSecs: null };
  }
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
  return (await fetchConsoleHealth(fetchImpl)).version;
}
