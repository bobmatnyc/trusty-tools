// Why: `installDaemonAuth` is the single point where every daemon request in
// this UI acquires its credential (#5439), so the properties that matter are
// exactly the ones a per-call-site implementation would get wrong: the header
// is attached without a call site asking, it is NOT attached to a non-bridge
// URL, and it never overwrites one a caller set deliberately.
// `openDaemonEventStream` is the stream counterpart; since #10166 it opens the
// stream through the Rust side's IPC `Channel` rather than an `EventSource`.
// What: drives both in Tauri mode against `tauri-fake.ts` (the mocked
// `@tauri-apps/api/core`) and a stub `fetch`, with the bridge at the
// `tcode-bridge://localhost` scheme origin.
// Test: this file.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  daemonToken,
  installDaemonAuth,
  openDaemonEventStream,
  resetDaemonTokenCache,
  sameOrigin,
} from './daemon-auth';
import { enterTauri, fakeTauri, leaveTauri } from './tauri-fake';

vi.mock('@tauri-apps/api/core', () => import('./tauri-fake'));

const TOKEN = 'a'.repeat(64);
const BASE = 'tcode-bridge://localhost';

/** Replace `globalThis.fetch` with a recording stub and return it. */
function stubFetch(response?: () => Response) {
  const stub = vi.fn(async () => (response ? response() : new Response('{}', { status: 200 })));
  globalThis.fetch = stub as unknown as typeof fetch;
  return stub;
}

/** A stub `fetch` that mints `tkt-1`, `tkt-2`, … in order. */
function stubMinting() {
  let minted = 0;
  return stubFetch(() => {
    minted += 1;
    return new Response(JSON.stringify({ ticket: `tkt-${minted}` }), { status: 200 });
  });
}

/** The `Authorization` value the wrapper passed through, if any. */
function authHeaderOf(stub: ReturnType<typeof stubFetch>, call = 0): string | null {
  const init = stub.mock.calls[call]?.[1] as RequestInit | undefined;
  return new Headers(init?.headers ?? {}).get('Authorization');
}

/** Let the subscription's awaited IPC calls settle. */
async function settle() {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

describe('daemon-auth', () => {
  beforeEach(() => {
    enterTauri();
    fakeTauri.reset();
    resetDaemonTokenCache();
    vi.restoreAllMocks();
  });

  afterEach(() => {
    leaveTauri();
  });

  it('reads the credential from the Rust side', async () => {
    expect(await daemonToken()).toBe(TOKEN);
  });

  it('has no credential outside Tauri, and ignores the retired localStorage override', async () => {
    leaveTauri();
    localStorage.setItem('trusty-code.daemonToken', TOKEN);
    expect(await daemonToken()).toBe('');
    localStorage.clear();
  });

  it('attaches the credential to a bridge request the call site did not authenticate', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${BASE}/sessions`);

    expect(stub).toHaveBeenCalledTimes(1);
    expect(authHeaderOf(stub)).toBe(`Bearer ${TOKEN}`);
  });

  it('leaves a non-bridge URL untouched', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch('https://example.test/anything');

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('does not overwrite an Authorization header the caller set', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${BASE}/sessions`, {
      headers: { Authorization: 'Bearer explicit' },
    });

    expect(authHeaderOf(stub)).toBe('Bearer explicit');
  });

  it('sends no header at all when there is no credential', async () => {
    fakeTauri.token = '';
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${BASE}/sessions`);

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('preserves method and body while adding the header', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${BASE}/tasks`, {
      method: 'POST',
      body: '{"task":"x"}',
    });

    const init = stub.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(init.body).toBe('{"task":"x"}');
    expect(authHeaderOf(stub)).toBe(`Bearer ${TOKEN}`);
  });

  it('matches the scheme origin exactly, and no other URL with an opaque origin', () => {
    // #10166: for a non-special scheme `URL.origin` is the opaque "null", so an
    // origin comparison would match ANY such URL. Protocol + host must match.
    expect(new URL(`${BASE}/sessions`).origin).toBe('null');
    expect(sameOrigin(`${BASE}/sessions`, BASE)).toBe(true);
    for (const hostile of [
      'evil-scheme://anywhere/sessions',
      'tcode-bridge://localhost.attacker.example/sessions',
      'tcode-bridge://elsewhere/sessions',
      'tcode-bridge:///sessions',
      'http://localhost/sessions',
      'tauri://localhost/sessions',
      'not a url',
    ]) {
      expect(sameOrigin(hostile, BASE), hostile).toBe(false);
    }
  });

  it('still rejects a lookalike host for an http base (#3280)', () => {
    const http = 'http://127.0.0.1:54321';
    expect(sameOrigin(`${http}/sessions`, http)).toBe(true);
    for (const hostile of [
      'http://127.0.0.1:54321.attacker.example/sessions',
      'https://127.0.0.1:54321/sessions',
      'http://127.0.0.1:54322/sessions',
    ]) {
      expect(sameOrigin(hostile, http), hostile).toBe(false);
    }
  });

  it('does not attach the credential to a URL with an opaque origin of its own', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch('evil-scheme://anywhere/sessions');

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('mints a ticket for the path and opens the stream through the shell with it', async () => {
    const stub = stubMinting();
    const seen: string[] = [];

    const stream = await openDaemonEventStream('/sessions/s1/events', (e) => seen.push(e.data));

    expect(stream).not.toBeNull();
    // The ticket request names the stream it is for, so the bridge can bind it.
    expect(String(stub.mock.calls[0]?.[0])).toBe(
      `${BASE}/auth/sse-ticket?path=%2Fsessions%2Fs1%2Fevents`,
    );
    expect(fakeTauri.streams).toHaveLength(1);
    expect(fakeTauri.streams[0].path).toBe('/sessions/s1/events');
    expect(fakeTauri.streams[0].ticket).toBe('tkt-1');

    fakeTauri.streams[0].emit({ seq: 1 });
    expect(seen).toEqual(['{"seq":1}']);
    stream?.close();
  });

  it('re-mints a fresh ticket and reopens when the stream ends', async () => {
    // A ticket is single-use and every end — a clean close or a failure — is a
    // drop to reconnect through, never a finished subscription.
    vi.useFakeTimers();
    try {
      stubMinting();
      const stream = await openDaemonEventStream('/sessions/s1/events', () => {});
      expect(fakeTauri.streams).toHaveLength(1);

      fakeTauri.streams[0].end('the stream failed: socket reset');

      await vi.advanceTimersByTimeAsync(1000);
      await settle();

      expect(fakeTauri.streams).toHaveLength(2);
      expect(fakeTauri.streams[1].ticket).toBe('tkt-2');
      stream?.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('ignores events from a superseded stream', async () => {
    vi.useFakeTimers();
    try {
      stubMinting();
      const seen: string[] = [];
      const stream = await openDaemonEventStream('/sessions/s1/events', (e) => seen.push(e.data));
      const first = fakeTauri.streams[0];
      first.end(null);
      await vi.advanceTimersByTimeAsync(1000);
      await settle();

      first.emit('late');
      fakeTauri.streams[1].emit('live');
      expect(seen).toEqual(['live']);
      stream?.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps retrying when a reconnect attempt fails, rather than dying silently', async () => {
    vi.useFakeTimers();
    try {
      let calls = 0;
      globalThis.fetch = vi.fn(async () => {
        calls += 1;
        if (calls === 2) throw new TypeError('Load failed');
        return new Response(JSON.stringify({ ticket: `tkt-${calls}` }), { status: 200 });
      }) as unknown as typeof fetch;
      const stream = await openDaemonEventStream('/sessions/s1/events', () => {});
      fakeTauri.streams[0].end(null);

      await vi.advanceTimersByTimeAsync(1000); // attempt 2: the mint rejects
      await settle();
      expect(fakeTauri.streams).toHaveLength(1);

      await vi.advanceTimersByTimeAsync(2000); // attempt 3: succeeds
      await settle();
      expect(fakeTauri.streams).toHaveLength(2);
      stream?.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('closes the live stream on the Rust side, and stops reconnecting once closed', async () => {
    vi.useFakeTimers();
    try {
      stubMinting();
      const stream = await openDaemonEventStream('/sessions/s1/events', () => {});
      stream?.close();
      expect(fakeTauri.closed).toEqual([fakeTauri.streams[0].id]);

      fakeTauri.streams[0].end(null);
      await vi.advanceTimersByTimeAsync(60000);
      expect(fakeTauri.streams).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('returns null when the bridge refuses to mint a ticket', async () => {
    stubFetch(() => new Response('', { status: 401 }));

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
    expect(fakeTauri.streams).toHaveLength(0);
  });

  it('returns null when the shell refuses to open the stream', async () => {
    stubMinting();
    fakeTauri.openError = '/sessions/s1/events did not open (502 Bad Gateway)';

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
  });

  it('returns null with no credential rather than opening an unauthenticated stream', async () => {
    fakeTauri.token = '';
    const stub = stubFetch();

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
    expect(stub).not.toHaveBeenCalled();
  });

  it('returns null outside Tauri, where there is nothing to open', async () => {
    leaveTauri();
    const stub = stubFetch();

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
    expect(stub).not.toHaveBeenCalled();
  });
});
