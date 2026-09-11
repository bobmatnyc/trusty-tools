// Chat-thread attachments in the browser (#7370).
//
// Why: a turn can now carry files, stored server-side under
// `<assistant home>/attachments/<session>/`. The browser never sees that path —
// it uploads to the session route, gets an id back, and refers to the file by
// that id from then on. Everything the chat view needs to DRAW a file (name,
// media type, size, the URL that fetches the bytes) comes from the server's own
// row, so nothing here re-derives a path or guesses a type.
//
// What: the wire types, the three REST calls, and the pure helpers the chat
// view uses — reading `[[attachment:<id>]]` markers back out of a persisted
// turn, hiding the rendered attachment blocks from the visible bubble, and
// slicing a CSV into a preview table.
//
// Test: `attachments.test.ts`.

import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

/** One stored attachment, exactly as the server's wire body describes it. */
export interface AttachmentRef {
  id: string;
  session_id: string;
  file_name: string;
  media_type: string;
  size: number;
  sha256: string;
  /** Path (not absolute URL) that serves the bytes. */
  url: string;
}

/**
 * The chat session id for an assistant.
 *
 * Why: it mirrors the server's `session_id_for` (`persona-{agent}`), which is
 * also what `GET /api/agents/:name/chat-history` reports as `session_id`. The
 * upload has to name a session BEFORE any history has been fetched, so the
 * format is reproduced here rather than waited for. A drift between the two
 * cannot corrupt anything: the send-side check resolves ids against the
 * session the SERVER derives, so a mismatched upload simply fails to resolve.
 * What: `persona-{agentId}`, with `ctrl` standing in for no roster selection —
 * the same default `stores/app.ts` documents for `activeAgentId`.
 * Test: `personaSessionId_mirrors_the_server_format`.
 */
export function personaSessionId(agentId: string | null | undefined): string {
  return `persona-${agentId ?? 'ctrl'}`;
}

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

function sessionRoute(agentId: string | null): string {
  const agent = agentId ?? 'ctrl';
  return `${apiBase()}/api/agents/${encodeURIComponent(agent)}/sessions/${encodeURIComponent(
    personaSessionId(agentId),
  )}/attachments`;
}

/** The absolute URL that serves one attachment's bytes. */
export function attachmentUrl(ref: AttachmentRef): string {
  return `${apiBase()}${ref.url}`;
}

/**
 * Upload files to the active assistant's chat session, in ONE request.
 *
 * Why: dropping three files is one gesture and should be one round trip. The
 * server accepts every file part and answers one row per file, having checked
 * every name and size before writing any of them — so a batch either lands
 * whole or not at all, and the client never has to reconcile a half-applied
 * upload of its own making.
 * What: `multipart/form-data` with one `file` part per file. Throws with the
 * server's own message on any non-2xx, because an upload that silently no-ops
 * leaves a user staring at a chat that never got their file. Validation is the
 * server's: a second copy of those rules here would drift from it.
 * Test: `uploadAttachments_posts_every_file_in_one_request`,
 * `uploadAttachments_throws_the_servers_message`.
 */
export async function uploadAttachments(
  agentId: string | null,
  files: File[],
): Promise<AttachmentRef[]> {
  const body = new FormData();
  for (const file of files) body.append('file', file, file.name);
  const r = await fetch(sessionRoute(agentId), {
    method: 'POST',
    headers: authHeaders(),
    body,
  });
  if (!r.ok) {
    let detail = `HTTP ${r.status}`;
    try {
      const failure = (await r.json()) as { error?: string };
      if (failure?.error) detail = failure.error;
    } catch {
      /* A non-JSON failure body leaves the status as the only detail. */
    }
    throw new Error(detail);
  }
  const payload = (await r.json()) as { attachments?: AttachmentRef[] };
  return Array.isArray(payload.attachments) ? payload.attachments : [];
}

/**
 * Every attachment stored in an assistant's chat session, by id.
 *
 * Why: rehydration reads markers out of persisted turns and needs the row
 * behind each one. One manifest read answers the whole thread.
 * What: an empty Map on any failure — a chat must render without its cards
 * rather than not render at all.
 * Test: `fetchSessionAttachments_indexes_rows_by_id`,
 * `fetchSessionAttachments_is_empty_when_unavailable`.
 */
export async function fetchSessionAttachments(
  agentId: string | null,
): Promise<Map<string, AttachmentRef>> {
  try {
    const r = await fetch(sessionRoute(agentId), { headers: authHeaders() });
    if (!r.ok) return new Map();
    const body = (await r.json()) as { attachments?: AttachmentRef[] };
    const rows = Array.isArray(body.attachments) ? body.attachments : [];
    return new Map(rows.map((row) => [row.id, row]));
  } catch {
    return new Map();
  }
}

/** Matches the server's `[[attachment:<id>]]` marker — 32 lowercase hex. */
const MARKER = /\[\[attachment:([0-9a-f]{32})\]\]/g;

/**
 * Attachment ids referenced by a turn's content, in order, without repeats.
 *
 * Why: the marker is what survives a reload — chat messages stay
 * `{role, content}` and the reference travels inside the content string.
 * Test: `parseAttachmentIds_reads_markers_in_order`.
 */
export function parseAttachmentIds(content: string): string[] {
  const seen: string[] = [];
  for (const match of content.matchAll(MARKER)) {
    if (!seen.includes(match[1])) seen.push(match[1]);
  }
  return seen;
}

/**
 * The part of a turn a human wrote, with the rendered attachment blocks removed.
 *
 * Why: the turn the model saw contains each attachment's contents inlined under
 * a fence, because the persisted turn and the delivered turn must not diverge.
 * The BUBBLE should not show that — the cards do. So the display strips from
 * the first marker onward, which is exactly where the server appended.
 * What: everything before the first marker, trimmed. A turn with no marker is
 * returned unchanged.
 * Test: `visibleText_strips_the_attachment_blocks`,
 * `visibleText_leaves_an_ordinary_turn_alone`.
 */
export function visibleText(content: string): string {
  const first = content.search(/\[\[attachment:[0-9a-f]{32}\]\]/);
  return first < 0 ? content : content.slice(0, first).trimEnd();
}

/** One parsed CSV preview: a header row plus up to `rows` body rows. */
export interface CsvPreview {
  header: string[];
  rows: string[][];
  /** Body rows that exist beyond the ones returned. */
  omitted: number;
}

/** Body rows a CSV card shows before it says how many more there are. */
export const CSV_PREVIEW_ROWS = 5;

/**
 * Slice a CSV into a small preview table.
 *
 * Why: a card shows the shape of a spreadsheet, not the spreadsheet. The
 * boundary matters — a file with exactly [`CSV_PREVIEW_ROWS`] body rows must
 * show all of them and report nothing omitted, while one more row must report
 * exactly one omitted.
 * What: a deliberately small splitter — it handles quoted fields containing
 * commas and doubled quotes, which is what a spreadsheet export actually
 * produces, and nothing else. A preview is not a parser; the model gets the
 * raw bytes.
 * Test: `csvPreview_shows_every_row_at_the_boundary`,
 * `csvPreview_reports_omitted_rows_past_the_boundary`,
 * `csvPreview_keeps_quoted_commas_together`.
 */
export function csvPreview(text: string, limit = CSV_PREVIEW_ROWS): CsvPreview {
  const lines = text.split(/\r?\n/).filter((line) => line.length > 0);
  if (lines.length === 0) return { header: [], rows: [], omitted: 0 };
  const header = splitCsvLine(lines[0]);
  const body = lines.slice(1);
  return {
    header,
    rows: body.slice(0, limit).map(splitCsvLine),
    omitted: Math.max(0, body.length - limit),
  };
}

function splitCsvLine(line: string): string[] {
  const cells: string[] = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < line.length; i += 1) {
    const c = line[i];
    if (quoted) {
      if (c === '"' && line[i + 1] === '"') {
        cell += '"';
        i += 1;
      } else if (c === '"') {
        quoted = false;
      } else {
        cell += c;
      }
    } else if (c === '"') {
      quoted = true;
    } else if (c === ',') {
      cells.push(cell);
      cell = '';
    } else {
      cell += c;
    }
  }
  cells.push(cell);
  return cells;
}

/** Human-readable byte size for a card's caption. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Whether a card should render an image thumbnail rather than an icon. */
export function isImage(ref: AttachmentRef): boolean {
  return ref.media_type.startsWith('image/');
}

/** Whether a card can show a text or CSV excerpt. */
export function isPreviewableText(ref: AttachmentRef): boolean {
  return (
    ref.media_type.startsWith('text/') ||
    ref.media_type === 'application/json' ||
    ref.media_type === 'application/x-ndjson'
  );
}
