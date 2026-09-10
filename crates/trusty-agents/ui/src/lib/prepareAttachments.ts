import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';
import { ATTACHMENT_LIMITS as limits, checkAttachmentBatch, type ChatAttachment } from './chatAttachments';
import type { ClipboardInput } from './clipboardAttachments';

function base64(bytes: Uint8Array): string {
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  return btoa(binary);
}

/** Why: task submission must share the API's normalization contract.
 * What: send original bytes or sanitized clipboard markup with authentication, return canonical content.
 * Test: prepareAttachments.test.ts verifies normalization, errors and malformed responses.
 */
export async function prepareAttachment(input: ClipboardInput, preview: ChatAttachment): Promise<ChatAttachment> {
  const item = 'file' in input
    ? { kind: 'file', name: input.file.name, mime_type: preview.kind === 'image' ? preview.mime_type : preview.source_format === 'xlsx' ? 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' : 'text/csv', data_base64: base64(new Uint8Array(await input.file.arrayBuffer())) }
    : { kind: 'clipboard', name: input.name, format: input.format === 'clipboard-html' ? 'html' : 'tsv', text: input.text };
  const token = getCurrentApiToken();
  const response = await fetch(`${apiBase()}/api/chat-attachments/prepare`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify({ items: [item] }), signal: AbortSignal.timeout(30_000),
  });
  const result = await response.json().catch(() => null);
  if (!response.ok) throw new Error(result?.error || `Attachment preparation failed (${response.status}).`);
  if (!Array.isArray(result?.attachments) || result.attachments.length !== 1 || !validAttachment(result.attachments[0])) throw new Error('Attachment preparation returned an invalid response.');
  return result.attachments[0];
}

function validAttachment(value: unknown): value is ChatAttachment {
  if (!value || typeof value !== 'object') return false;
  const item = value as ChatAttachment;
  if (typeof item.name !== 'string') return false;
  if (item.kind === 'image') return ['image/png','image/jpeg','image/webp'].includes(item.mime_type) && typeof item.data_base64 === 'string' && item.data_base64.length <= Math.ceil(limits.fileBytes / 3) * 4;
  if (item.kind !== 'table' || !['clipboard-html','clipboard-tsv','csv','xlsx'].includes(item.source_format) || !Array.isArray(item.sheets) || !item.sheets.length) return false;
  if (!item.sheets.every(sheet => typeof sheet.name === 'string' && Array.isArray(sheet.rows) && sheet.rows.length <= limits.rows && sheet.rows.every(row => Array.isArray(row) && row.length <= limits.columns && row.every(cell => typeof cell === 'string')))) return false;
  try { checkAttachmentBatch([{ id: '', sourceBytes: 0, attachment: item }]); return true; } catch { return false; }
}
