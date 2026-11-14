// Why: since #10166 the webview reaches the bridge only through Tauri — a
// custom URI scheme for `fetch()` and an IPC `Channel` for the two event
// streams — so a test that exercises the real transport has to run in Tauri
// mode. This is the stand-in for `@tauri-apps/api/core` those tests mock in.
// TEST-ONLY: nothing in the app imports it, so `vite build` never bundles it.
//
// What: `invoke` answers the four bridge commands from `fakeTauri`'s state;
// `Channel` is a bare holder for `onmessage`; `enterTauri`/`leaveTauri` flip
// the `__TAURI_INTERNALS__` probe `isTauri()` reads. A test file wires it with
//   vi.mock('@tauri-apps/api/core', () => import('<path>/tauri-fake'));
// Test: used by `daemon-auth.test.ts`, `api-config.test.ts`,
// `WorkstreamActivity.test.ts` and `WorkstreamSwitcher.test.ts`.
import { vi } from 'vitest';

/** One `open_daemon_stream` call the fake answered. */
export interface FakeStream {
  id: number;
  path: string;
  ticket: string;
  channel: Channel<unknown>;
  /** Push one SSE payload through the channel, as the Rust pump does. */
  emit(data: unknown): void;
  /** End the stream, as the Rust pump does when the body closes. */
  end(error?: string | null): void;
}

/** The fake's state; `reset()` restores the defaults between tests. */
export const fakeTauri = {
  url: 'tcode-bridge://localhost',
  token: 'a'.repeat(64),
  /** When set, `open_daemon_stream` rejects with this message. */
  openError: null as string | null,
  streams: [] as FakeStream[],
  closed: [] as number[],
  reset() {
    this.url = 'tcode-bridge://localhost';
    this.token = 'a'.repeat(64);
    this.openError = null;
    this.streams = [];
    this.closed = [];
    invoke.mockClear();
  },
};

/** Stand-in for `@tauri-apps/api/core`'s `Channel`. */
export class Channel<T> {
  onmessage: (message: T) => void = () => {};
}

/** Stand-in for `@tauri-apps/api/core`'s `invoke`. */
export const invoke = vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
  switch (cmd) {
    case 'get_daemon_url':
      return fakeTauri.url;
    case 'get_daemon_token':
      return fakeTauri.token;
    case 'open_daemon_stream': {
      if (fakeTauri.openError) throw fakeTauri.openError;
      const channel = args.onEvent as Channel<unknown>;
      const stream: FakeStream = {
        id: fakeTauri.streams.length + 1,
        path: String(args.path),
        ticket: String(args.ticket),
        channel,
        emit(data) {
          channel.onmessage({
            kind: 'message',
            data: typeof data === 'string' ? data : JSON.stringify(data),
          });
        },
        end(error = null) {
          channel.onmessage({ kind: 'end', error });
        },
      };
      fakeTauri.streams.push(stream);
      return stream.id;
    }
    case 'close_daemon_stream':
      fakeTauri.closed.push(Number(args.id));
      return true;
    default:
      throw new Error(`unexpected invoke: ${cmd}`);
  }
});

/** Make `isTauri()` true for the current test. */
export function enterTauri(): void {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}

/** Make `isTauri()` false again. */
export function leaveTauri(): void {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}
