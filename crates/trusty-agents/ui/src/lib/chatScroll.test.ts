// Unit tests for the chat scroll-anchoring predicate (PR #3895 code-critic
// HIGH-3). See `chatScroll.ts` for why ChatView stopped forcing the scroll
// unconditionally.

import { describe, expect, it } from 'vitest';
import { BOTTOM_EPSILON_PX, isPinnedToBottom } from './chatScroll';

describe('isPinnedToBottom', () => {
  it('is true when the reader is parked at the very bottom', () => {
    expect(isPinnedToBottom({ scrollTop: 800, scrollHeight: 1000, clientHeight: 200 })).toBe(true);
  });

  it('is true within the epsilon of slack', () => {
    // 10px short of the end — a reader here means "keep me at the newest".
    expect(isPinnedToBottom({ scrollTop: 790, scrollHeight: 1000, clientHeight: 200 })).toBe(true);
    expect(BOTTOM_EPSILON_PX).toBeGreaterThan(10);
  });

  it('is false once the reader has scrolled up to read history', () => {
    expect(isPinnedToBottom({ scrollTop: 100, scrollHeight: 1000, clientHeight: 200 })).toBe(false);
  });

  it('is true for a container with nothing to scroll (an empty chat)', () => {
    expect(isPinnedToBottom({ scrollTop: 0, scrollHeight: 0, clientHeight: 0 })).toBe(true);
    expect(isPinnedToBottom({ scrollTop: 0, scrollHeight: 200, clientHeight: 200 })).toBe(true);
  });

  it('honours an explicit epsilon', () => {
    const metrics = { scrollTop: 780, scrollHeight: 1000, clientHeight: 200 };
    expect(isPinnedToBottom(metrics, 5)).toBe(false);
    expect(isPinnedToBottom(metrics, 50)).toBe(true);
  });
});
