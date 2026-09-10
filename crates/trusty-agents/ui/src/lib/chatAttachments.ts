// #7370: one bounded wire representation for native/browser task submission.
export type TableFormat = 'clipboard-html' | 'clipboard-tsv' | 'csv' | 'xlsx';
export type ChatAttachment =
  | { kind: 'image'; name: string; mime_type: 'image/png' | 'image/jpeg' | 'image/webp'; data_base64: string }
  | { kind: 'table'; name: string; source_format: TableFormat; sheets: { name: string; rows: string[][] }[] };
export type StoredChatAttachment = Exclude<ChatAttachment, {kind: 'image'}> | {kind: 'image'; name: string; mime_type: 'image/png' | 'image/jpeg' | 'image/webp'; asset_id: string};
export type DisplayChatAttachment = ChatAttachment | StoredChatAttachment;
export interface DraftAttachment { id: string; sourceBytes: number; attachment: ChatAttachment }
export const ATTACHMENT_LIMITS = { count: 4, fileBytes: 5 * 1024 * 1024, totalBytes: 10 * 1024 * 1024, expandedBytes: 16 * 1024 * 1024, sheets: 3, rows: 200, columns: 30, characters: 50_000, imagePixels: 24_000_000, imageSide: 8192, workerMs: 5000 } as const;
export function checkAttachmentBatch(items: DraftAttachment[]): void {
  if (items.length > ATTACHMENT_LIMITS.count) throw new Error('Attach at most four images or tables per message.');
  if (items.some(item => item.sourceBytes > ATTACHMENT_LIMITS.fileBytes) || items.reduce((sum, item) => sum + item.sourceBytes, 0) > ATTACHMENT_LIMITS.totalBytes) throw new Error('Attachments must be at most 5 MiB each and 10 MiB per message.');
  const tables = items.flatMap(item => item.attachment.kind === 'table' ? item.attachment.sheets : []);
  const characters = tables.reduce((total, sheet) => total + sheet.rows.reduce((sum, row) => sum + row.reduce((n, cell) => n + cell.length, 0), 0), 0);
  if (tables.length > ATTACHMENT_LIMITS.sheets || characters > ATTACHMENT_LIMITS.characters) throw new Error('A message supports at most three sheets and 50,000 table characters.');
}
export function attachmentSummary(attachment: DisplayChatAttachment): string {
  return attachment.kind === 'image' ? attachment.name : `${attachment.name} · ${attachment.sheets.map(sheet => `${sheet.name}: ${sheet.rows.length} rows`).join(', ')}`;
}
