import { beforeEach, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
vi.mock('../lib/clipboardAttachments', () => ({ parseClipboardInput: vi.fn() }));
import { parseClipboardInput } from '../lib/clipboardAttachments';
import { composerDrafts, addClipboardInputs, retryClipboardInputs, setDraftText, clearSubmittedDraft, preserveFailedDraft, restoreFailedDraft } from './composerDrafts';
const item = { id:'table',sourceBytes:10,attachment:{kind:'table' as const,name:'table',source_format:'csv' as const,sheets:[{name:'Sheet1',rows:[['A']]}]}};
beforeEach(() => { composerDrafts.set({}); vi.resetAllMocks(); });
it('late paste stays in its originating chat, and cleared drafts reject stale paste', async () => {
  let finish!: (value: typeof item) => void;
  vi.mocked(parseClipboardInput).mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
  const pending = addClipboardInputs('alice', [{name:'table',format:'csv',text:'A'}]);
  setDraftText('bob', 'new chat'); finish(item); await pending;
  expect(get(composerDrafts).alice.items).toEqual([item]);
  expect(get(composerDrafts).bob.items).toEqual([]);
  vi.mocked(parseClipboardInput).mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
  const stale = addClipboardInputs('alice', [{name:'table',format:'csv',text:'A'}]);
  clearSubmittedDraft('alice'); finish(item); await stale;
  expect(get(composerDrafts).alice.items).toEqual([]);
});
it('restores a failed submission without discarding a newer draft', () => {
  setDraftText('alice', 'new edits'); preserveFailedDraft('alice', 'failed text', [item]);
  restoreFailedDraft('alice');
  expect(get(composerDrafts).alice.text).toBe('new edits\nfailed text');
  expect(get(composerDrafts).alice.items).toEqual([item]);
  restoreFailedDraft('alice'); expect(get(composerDrafts).alice.items).toHaveLength(1);
});
it('retains pasted inputs and edited text after preparation fails, then retries canonically', async () => {
  const input = {name:'table',format:'clipboard-tsv' as const,text:'A\tB'};
  vi.mocked(parseClipboardInput).mockRejectedValueOnce(new Error('API unavailable')).mockResolvedValueOnce(item);
  setDraftText('alice','editable text');
  await addClipboardInputs('alice',[input]);
  expect(get(composerDrafts).alice).toMatchObject({text:'editable text',items:[],pendingPaste:[input],error:'API unavailable'});
  retryClipboardInputs('alice');
  await vi.waitFor(() => expect(get(composerDrafts).alice.items).toEqual([item]));
  expect(get(composerDrafts).alice.pendingPaste).toBeUndefined();
});
it.each(['valid','oversized'])('keeps failed paste A when a %s paste B arrives', async kind => {
  const first = {name:'A',format:'clipboard-tsv' as const,text:'A\tB'};
  const next = {name:'B',format:'clipboard-tsv' as const,text:kind === 'valid' ? 'C\tD' : 'x'.repeat(6 * 1024 * 1024)};
  vi.mocked(parseClipboardInput).mockRejectedValueOnce(new Error('Preparation unavailable')).mockResolvedValue(item);
  setDraftText('alice','keep this text');
  await addClipboardInputs('alice',[first]);
  await addClipboardInputs('alice',[next]);
  expect(get(composerDrafts).alice).toMatchObject({text:'keep this text',items:[],pendingPaste:[first]});
  expect(vi.mocked(parseClipboardInput)).toHaveBeenCalledTimes(1);
  retryClipboardInputs('alice');
  await vi.waitFor(() => expect(get(composerDrafts).alice.items).toEqual([item]));
  expect(vi.mocked(parseClipboardInput)).toHaveBeenLastCalledWith(first);
});
