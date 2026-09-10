import { describe, expect, it } from 'vitest';
import { renderChatMarkdown, assistantEmptyNotice } from './chatRendering';

describe('chat markdown display boundary', () => {
  it('removes author styling and interactive attributes that could escape the message', () => {
    const root = document.createElement('div');
    root.innerHTML = renderChatMarkdown('<div class="fixed inset-0 z-50" style="position:fixed" id="app" role="button" tabindex="0" data-local-href="secret" aria-label="System control">Content</div>');
    expect(root.textContent).toBe('Content');
    expect(root.querySelector('[class],[style],[id],[role],[tabindex],[data-local-href],[aria-label]')).toBeNull();
  });
  it('removes legacy network image attributes and raw HTML controls', () => {
    const root = document.createElement('div');
    root.innerHTML = renderChatMarkdown('<table background="https://host/pixel"><tr><td background="https://host/cell">x</td></tr></table><select><option>Choice</option></select><label for="app">Label</label><div popover="auto">Text</div>');
    expect(root.querySelector('table td')?.textContent).toBe('x');
    expect(root.querySelector('[background],select,option,label,[for],[popover]')).toBeNull();
    expect(root.querySelectorAll('[src],[href],[style],[class]')).toHaveLength(0);
  });
  it('keeps generated emphasis and code readable without author classes', () => {
    const root = document.createElement('div');
    root.innerHTML = renderChatMarkdown('**Answer**\n\n```js\nconst value = 1;\n```');
    expect(root.querySelector('strong')?.textContent).toBe('Answer');
    expect(root.querySelector('pre code')?.textContent).toContain('const value = 1;');
    expect(root.querySelector('[class]')).toBeNull();
  });
});

it('scopes empty-response tool evidence to the exact task and distinguishes tool-only completion', () => {
  const assistant = { id:'a', role:'assistant' as const, taskId:'own', content:'', timestamp:0 };
  const completed = { id:'t', role:'tool' as const, taskId:'own', content:'', timestamp:0, activityStatus:'complete' as const };
  const otherError = { ...completed, id:'other', taskId:'other', activityStatus:'error' as const };
  expect(assistantEmptyNotice(assistant, [completed, otherError])).toBe('Tool activities completed. No written response was returned.');
  expect(assistantEmptyNotice(assistant, [otherError])).toBe('No response was returned.');
  expect(assistantEmptyNotice({ ...assistant, taskId:undefined }, [completed, otherError])).toBe('No response was returned.');
});
