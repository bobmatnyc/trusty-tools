// The bearer a channel WRITE must carry (#7609).
//
// Why: a channel write is the one operation this API gates on a credential even
// from loopback, because whoever can write a binding can redirect every
// assistant's inbound traffic. On a daemon started without `--api-token` that
// credential is minted per boot and published on the unauthenticated
// `/api/config` probe — to a same-origin caller only, so the UI the daemon
// serves can save while a page from anywhere else cannot read the value. Its
// own module because BOTH the Channels tab and the (deprecated) Listeners tab
// write through the gate, and a copy in each would drift.
//
// What: the probe result is cached for the page's lifetime, but ONLY on
// success. A failed probe is never cached (critic MEDIUM-5): the daemon may
// simply not have been up yet, and caching '' would leave every later save
// 401ing with no way back short of a reload. `withChannelWriteAuth` retries a
// 401 exactly once against a freshly probed credential, which covers the daemon
// that restarted — and minted a new credential — under a long-lived page.
//
// Test: `channel-auth.test.ts`.

import { apiBase } from './api-config';

let cached: string | null = null;

/** Forget the cached credential, so the next write re-probes. */
export function resetChannelWriteToken(): void {
  cached = null;
}

async function probe(): Promise<string> {
  if (cached !== null) return cached;
  try {
    const r = await fetch(`${apiBase()}/api/config`);
    if (!r.ok) return '';
    const cfg = (await r.json()) as { channel_write_token?: string };
    const value = typeof cfg.channel_write_token === 'string' ? cfg.channel_write_token : '';
    // Only a successful probe is cached; '' from a failure must not stick.
    if (value) cached = value;
    return value;
  } catch {
    return '';
  }
}

/** The `Authorization` header for a channel write, or none when unavailable. */
export async function channelWriteAuth(): Promise<Record<string, string>> {
  const token = await probe();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * Run `send` with the channel-write credential attached, retrying once on 401
 * against a freshly probed one.
 *
 * Why: the credential is per-boot, so a daemon restart under a long-lived page
 * invalidates the cached value and every later save would 401 forever.
 */
export async function withChannelWriteAuth<T>(
  send: (headers: Record<string, string>) => Promise<T>,
): Promise<T> {
  try {
    return await send(await channelWriteAuth());
  } catch (e) {
    // #7609: branch on the STATUS, never on the message. The daemon's refusal
    // reads "Channel writes require an API token…" — it contains neither "401"
    // nor "unauthorized", so a text predicate matched nothing and the retry
    // never ran. `tmApi` attaches `status` for exactly this.
    if ((e as { status?: number } | null)?.status !== 401) throw e;
    resetChannelWriteToken();
    return send(await channelWriteAuth());
  }
}
