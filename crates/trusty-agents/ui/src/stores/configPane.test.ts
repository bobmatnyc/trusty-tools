// Unit tests for the agent-configuration takeover state (#3894, epic #3052).

import { beforeEach, describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import {
  closeConfigPane,
  configPaneOpen,
  isConfigExitKey,
  openConfigPane,
  toggleConfigPane,
} from './configPane';

beforeEach(() => {
  closeConfigPane();
});

describe('configPaneOpen', () => {
  it('starts closed — chat is the default surface', () => {
    expect(get(configPaneOpen)).toBe(false);
  });

  it('open/close/toggle move the takeover between its two states', () => {
    openConfigPane();
    expect(get(configPaneOpen)).toBe(true);

    // Opening an already-open pane is a no-op, not a toggle: the gear button
    // is not the only opener, and a second open must never close.
    openConfigPane();
    expect(get(configPaneOpen)).toBe(true);

    closeConfigPane();
    expect(get(configPaneOpen)).toBe(false);

    toggleConfigPane();
    expect(get(configPaneOpen)).toBe(true);
    toggleConfigPane();
    expect(get(configPaneOpen)).toBe(false);
  });
});

describe('isConfigExitKey', () => {
  const key = (
    k: string,
    mods: Partial<Pick<KeyboardEvent, 'ctrlKey' | 'metaKey' | 'altKey'>> = {},
  ) => ({ key: k, ctrlKey: false, metaKey: false, altKey: false, ...mods });

  it('isConfigExitKey_true_for_plain_escape', () => {
    expect(isConfigExitKey(key('Escape'))).toBe(true);
  });

  it('isConfigExitKey_false_for_other_keys', () => {
    for (const k of ['Enter', 'Esc', 'e', 'Tab', 'Backspace']) {
      expect(isConfigExitKey(key(k))).toBe(false);
    }
  });

  it('isConfigExitKey_false_when_chorded_with_a_platform_modifier', () => {
    expect(isConfigExitKey(key('Escape', { metaKey: true }))).toBe(false);
    expect(isConfigExitKey(key('Escape', { ctrlKey: true }))).toBe(false);
    expect(isConfigExitKey(key('Escape', { altKey: true }))).toBe(false);
  });
});
