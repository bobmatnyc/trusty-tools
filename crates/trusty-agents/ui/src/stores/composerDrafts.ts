// #7370: drafts live in memory, keyed by conversation; binary data never enters localStorage.
import { get, writable } from 'svelte/store';
import { checkAttachmentBatch, type DraftAttachment } from '../lib/chatAttachments';
import { parseClipboardInput, type ClipboardInput } from '../lib/clipboardAttachments';
export interface ComposerDraft { text: string; items: DraftAttachment[]; busy: number; error: string; epoch: number; pendingPaste?: ClipboardInput[]; failed?: { text: string; items: DraftAttachment[] } }
export const emptyDraft: ComposerDraft = { text: '', items: [], busy: 0, error: '', epoch: 0 };
export const composerDrafts = writable<Record<string, ComposerDraft>>({});
function change(key: string, update: (draft: ComposerDraft) => ComposerDraft) {
  composerDrafts.update(all => ({ ...all, [key]: update(all[key] ?? emptyDraft) }));
}
export const setDraftText = (key: string, text: string) => change(key, draft => ({ ...draft, text }));
export const setDraftError = (key: string, error: string) => change(key, draft => ({ ...draft, error }));
export const discardPendingPaste = (key: string) => change(key, draft => draft.busy ? draft : ({ ...draft, pendingPaste: undefined, error: '' }));
export const removeDraftAttachment = (key: string, id: string) => change(key, draft => ({ ...draft, items: draft.items.filter(item => item.id !== id) }));
export const clearSubmittedDraft = (key: string) => change(key, draft => ({ ...emptyDraft, epoch: draft.epoch + 1 }));
export const preserveFailedDraft = (key: string, text: string, items: DraftAttachment[]) => change(key, draft => ({ ...draft, failed: { text, items } }));
export function restoreFailedDraft(key: string): void {
  change(key, draft => {
    if (!draft.failed) return draft;
    const items = [...draft.items, ...draft.failed.items.filter(item => !draft.items.some(current => current.id === item.id))];
    try {
      checkAttachmentBatch(items);
      return { ...draft, text: [draft.text, draft.failed.text].filter(Boolean).join('\n'), items, failed: undefined, error: '' };
    } catch (error) { return { ...draft, error: String(error) }; }
  });
}
export async function addClipboardInputs(key: string, inputs: ClipboardInput[]): Promise<void> {
  const draft = get(composerDrafts)[key] ?? emptyDraft;
  if (draft.pendingPaste || draft.busy) {
    change(key, value => ({ ...value, error: value.pendingPaste ? 'Retry or discard the previous failed paste before pasting more attachments.' : 'Wait for the current pasted files to finish before pasting more attachments.' }));
    return;
  }
  await prepareClipboardInputs(key, inputs);
}
async function prepareClipboardInputs(key: string, inputs: ClipboardInput[]): Promise<void> {
  const sizes = inputs.map(input => 'file' in input ? input.file.size : new TextEncoder().encode(input.text).length);
  if (inputs.length > 4 || sizes.some(size => size > 5 * 1024 * 1024) || sizes.reduce((sum, size) => sum + size, 0) > 10 * 1024 * 1024) {
    change(key, draft => ({ ...draft, error: 'Paste at most four files, 5 MiB each and 10 MiB total.', pendingPaste: undefined }));
    return;
  }
  const epoch = (get(composerDrafts)[key] ?? emptyDraft).epoch;
  change(key, draft => ({ ...draft, busy: draft.busy + 1, error: '' }));
  try {
    const items: DraftAttachment[] = [];
    for (const input of inputs) items.push(await parseClipboardInput(input));
    change(key, draft => {
      if (draft.epoch !== epoch) return draft;
      const next = [...draft.items, ...items];
      checkAttachmentBatch(next);
      return { ...draft, items: next, pendingPaste: undefined, error: '' };
    });
  } catch (error) { change(key, draft => draft.epoch === epoch ? { ...draft, pendingPaste: inputs, error: error instanceof Error ? error.message : String(error) } : draft); }
  finally { change(key, draft => draft.epoch === epoch ? { ...draft, busy: Math.max(0, draft.busy - 1) } : draft); }
}
export function retryClipboardInputs(key: string): void {
  const draft = get(composerDrafts)[key];
  if (draft?.pendingPaste && !draft.busy) void prepareClipboardInputs(key, draft.pendingPaste);
}
