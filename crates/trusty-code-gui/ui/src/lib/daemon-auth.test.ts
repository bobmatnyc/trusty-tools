// Why: `installDaemonAuth` is the single point where every daemon request in
// this UI acquires its credential (#5439), so the properties that matter are
// exactly the ones a per-call-site implementation would get wrong: the header
// is attached without a call site asking, it is NOT attached to a non-daemon
// URL, and it never overwrites one a caller set deliberately.
// What: drives the wrapper against a stub `fetch`, with the daemon URL and
// credential supplied through localStorage (the plain-browser path — `isTauri()`
// is false under jsdom).
// Test: this file.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { DEFAULT_DAEMON_URL } from './api-config';
import {
  TICKET_QUERY_PARAM,
  TOKEN_STORAGE_KEY,
  daemonToken,
  installDaemonAuth,
  openDaemonEventStream,
  resetDaemonTokenCache,
  sameOrigin,
} from './daemon-auth';

const TOKEN = 'a'.repeat(64);

/** Replace `globalThis.fetch` with a recording stub and return it. */
function stubFetch(response?: Response) {
  const stub = vi.fn(async () => response ?? new Response('{}', { status: 200 }));
  globalThis.fetch = stub as unknown as typeof fetch;
  return stub;
}

/** The `Authorization` value the wrapper passed through, if any. */
function authHeaderOf(stub: ReturnType<typeof stubFetch>, call = 0): string | null {
  const init = stub.mock.calls[call]?.[1] as RequestInit | undefined;
  return new Headers(init?.headers ?? {}).get('Authorization');
}

describe('daemon-auth', () => {
  beforeEach(() => {
    localStorage.clear();
    resetDaemonTokenCache();
    vi.restoreAllMocks();
  });

  it('reads the credential from the localStorage override in a plain browser', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    expect(await daemonToken()).toBe(TOKEN);
  });

  it('resolves an empty credential when none is stored', async () => {
    expect(await daemonToken()).toBe('');
  });

  it('attaches the credential to a daemon request the call site did not authenticate', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${DEFAULT_DAEMON_URL}/sessions`);

    expect(stub).toHaveBeenCalledTimes(1);
    expect(authHeaderOf(stub)).toBe(`Bearer ${TOKEN}`);
  });

  it('leaves a non-daemon URL untouched', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const stub = stubFetch();
    installDaemonAuth();

    await fetch('https://example.test/anything');

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('does not overwrite an Authorization header the caller set', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${DEFAULT_DAEMON_URL}/sessions`, {
      headers: { Authorization: 'Bearer explicit' },
    });

    expect(authHeaderOf(stub)).toBe('Bearer explicit');
  });

  it('sends no header at all when there is no credential', async () => {
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${DEFAULT_DAEMON_URL}/sessions`);

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('preserves method and body while adding the header', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${DEFAULT_DAEMON_URL}/tasks`, {
      method: 'POST',
      body: '{"task":"x"}',
    });

    const init = stub.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe('POST');
    expect(init.body).toBe('{"task":"x"}');
    expect(authHeaderOf(stub)).toBe(`Bearer ${TOKEN}`);
  });

  it('rejects a lookalike host rather than prefix-matching the base URL', () => {
    // The prefix-match class already fixed once in #3280:
    // `http://127.0.0.1:7882` is a PREFIX of
    // `http://127.0.0.1:7882.attacker.example`, so a startsWith test would have
    // sent the credential there.
    expect(sameOrigin(`${DEFAULT_DAEMON_URL}/sessions`, DEFAULT_DAEMON_URL)).toBe(true);
    for (const hostile of [
      'http://127.0.0.1:7882.attacker.example/sessions',
      'http://127.0.0.1:78820/sessions',
      'https://127.0.0.1:7882/sessions',
      'http://127.0.0.1:7883/sessions',
      'not a url',
    ]) {
      expect(sameOrigin(hostile, DEFAULT_DAEMON_URL)).toBe(false);
    }
  });

  it('does not attach the credential to a lookalike prefix host', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const stub = stubFetch();
    installDaemonAuth();

    await fetch(`${DEFAULT_DAEMON_URL}.attacker.example/sessions`);

    expect(authHeaderOf(stub)).toBeNull();
  });

  it('mints a ticket for the requested path and puts it in the SSE URL', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    const { opened, FakeEventSource } = fakeEventSource();
    vi.stubGlobal('EventSource', FakeEventSource);
    const stub = stubFetch(new Response(JSON.stringify({ ticket: 'tkt-1' }), { status: 200 }));

    const stream = await openDaemonEventStream('/sessions/s1/events', () => {});

    expect(stream).not.toBeNull();
    // The ticket request names the stream it is for, so the daemon can bind it.
    expect(String(stub.mock.calls[0]?.[0])).toBe(
      `${DEFAULT_DAEMON_URL}/auth/sse-ticket?path=%2Fsessions%2Fs1%2Fevents`,
    );
    expect(opened).toHaveLength(1);
    expect(opened[0]).toBe(
      `${DEFAULT_DAEMON_URL}/sessions/s1/events?${TICKET_QUERY_PARAM}=tkt-1`,
    );
    // The durable credential must never reach the URL — that is the whole
    // reason the ticket exchange exists.
    expect(opened[0]).not.toContain(TOKEN);
  });

  it('re-mints a fresh ticket when the stream drops', async () => {
    // A ticket is single-use, so EventSource's OWN reconnect re-sends a spent
    // one, gets 401, and the stream dies permanently. Only this layer can mint
    // a replacement.
    vi.useFakeTimers();
    try {
      localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
      const { opened, instances, FakeEventSource } = fakeEventSource();
      vi.stubGlobal('EventSource', FakeEventSource);
      let minted = 0;
      globalThis.fetch = vi.fn(async () => {
        minted += 1;
        return new Response(JSON.stringify({ ticket: `tkt-${minted}` }), { status: 200 });
      }) as unknown as typeof fetch;

      const stream = await openDaemonEventStream('/sessions/s1/events', () => {});
      expect(opened).toHaveLength(1);

      instances[0].onerror?.(new Event('error'));
      expect(instances[0].closed).toBe(true);

      await vi.advanceTimersByTimeAsync(1000);
      await vi.advanceTimersByTimeAsync(0);

      expect(opened).toHaveLength(2);
      expect(opened[1]).toContain(`${TICKET_QUERY_PARAM}=tkt-2`);
      expect(opened[1]).not.toContain('tkt-1');

      stream?.close();
    } finally {
      vi.useRealTimers();
    }
  });

  it('stops reconnecting once closed', async () => {
    vi.useFakeTimers();
    try {
      localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
      const { opened, instances, FakeEventSource } = fakeEventSource();
      vi.stubGlobal('EventSource', FakeEventSource);
      stubFetch(new Response(JSON.stringify({ ticket: 'tkt' }), { status: 200 }));

      const stream = await openDaemonEventStream('/sessions/s1/events', () => {});
      instances[0].onerror?.(new Event('error'));
      stream?.close();

      await vi.advanceTimersByTimeAsync(60000);
      expect(opened).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it('returns null when the daemon refuses to mint a ticket', async () => {
    localStorage.setItem(TOKEN_STORAGE_KEY, TOKEN);
    vi.stubGlobal('EventSource', fakeEventSource().FakeEventSource);
    stubFetch(new Response('', { status: 401 }));

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
  });

  it('returns null with no credential rather than opening an unauthenticated stream', async () => {
    vi.stubGlobal('EventSource', fakeEventSource().FakeEventSource);
    const stub = stubFetch();

    expect(await openDaemonEventStream('/sessions/s1/events', () => {})).toBeNull();
    expect(stub).not.toHaveBeenCalled();
  });
});

/** A recording `EventSource` double: the URLs opened and each live instance. */
function fakeEventSource() {
  const opened: string[] = [];
  const instances: FakeSource[] = [];
  class FakeSource {
    onmessage: ((e: MessageEvent) => void) | null = null;
    onerror: ((e: Event) => void) | null = null;
    closed = false;
    constructor(url: string) {
      opened.push(url);
      instances.push(this);
    }
    close() {
      this.closed = true;
    }
  }
  return { opened, instances, FakeEventSource: FakeSource };
}
