import { describe, expect, it, vi, afterEach } from 'vitest';
import {
  CSV_PREVIEW_ROWS,
  csvPreview,
  fetchSessionAttachments,
  formatSize,
  parseAttachmentIds,
  personaSessionId,
  uploadAttachments,
  visibleText,
  type AttachmentRef,
} from './attachments';

const ID = 'a'.repeat(32);
const OTHER = 'b'.repeat(32);

function row(overrides: Partial<AttachmentRef> = {}): AttachmentRef {
  return {
    id: ID,
    session_id: 'persona-izzie',
    file_name: 'data.csv',
    media_type: 'text/csv',
    size: 12,
    sha256: 'deadbeef',
    url: `/api/agents/izzie/sessions/persona-izzie/attachments/${ID}`,
    ...overrides,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('personaSessionId', () => {
  it('personaSessionId_mirrors_the_server_format', () => {
    expect(personaSessionId('izzie')).toBe('persona-izzie');
    // `activeAgentId` is null for the base ctrl session (stores/app.ts).
    expect(personaSessionId(null)).toBe('persona-ctrl');
  });
});

describe('markers', () => {
  it('parseAttachmentIds_reads_markers_in_order', () => {
    const content = `look\n\n[[attachment:${ID}]] a.csv\n[[attachment:${OTHER}]] b.png\n[[attachment:${ID}]]`;
    expect(parseAttachmentIds(content)).toEqual([ID, OTHER]);
  });

  it('parseAttachmentIds_ignores_malformed_markers', () => {
    expect(parseAttachmentIds('no markers here')).toEqual([]);
    expect(parseAttachmentIds('[[attachment:nope]]')).toEqual([]);
    expect(parseAttachmentIds(`[[attachment:${ID.toUpperCase()}]]`)).toEqual([]);
  });

  it('visibleText_strips_the_attachment_blocks', () => {
    const content = `what is in this?\n\n[[attachment:${ID}]] data.csv (text/csv, 8 bytes)\n\`\`\`csv\na,b\n1,2\n\`\`\``;
    expect(visibleText(content)).toBe('what is in this?');
  });

  it('visibleText_leaves_an_ordinary_turn_alone', () => {
    expect(visibleText('just a message')).toBe('just a message');
  });
});

describe('csvPreview', () => {
  const header = 'a,b';
  const body = (n: number) =>
    Array.from({ length: n }, (_, i) => `${i},${i}`).join('\n');

  it('csvPreview_shows_every_row_at_the_boundary', () => {
    const preview = csvPreview(`${header}\n${body(CSV_PREVIEW_ROWS)}`);
    expect(preview.header).toEqual(['a', 'b']);
    expect(preview.rows).toHaveLength(CSV_PREVIEW_ROWS);
    expect(preview.omitted).toBe(0);
  });

  it('csvPreview_reports_omitted_rows_past_the_boundary', () => {
    const preview = csvPreview(`${header}\n${body(CSV_PREVIEW_ROWS + 1)}`);
    expect(preview.rows).toHaveLength(CSV_PREVIEW_ROWS);
    expect(preview.omitted).toBe(1);
  });

  it('csvPreview_keeps_quoted_commas_together', () => {
    const preview = csvPreview('name,note\n"Doe, Jane","said ""hi"""');
    expect(preview.rows[0]).toEqual(['Doe, Jane', 'said "hi"']);
  });

  it('csvPreview_handles_an_empty_file', () => {
    expect(csvPreview('')).toEqual({ header: [], rows: [], omitted: 0 });
  });
});

describe('formatSize', () => {
  it('formatSize_scales_to_the_unit', () => {
    expect(formatSize(512)).toBe('512 B');
    expect(formatSize(2048)).toBe('2.0 KB');
    expect(formatSize(5 * 1024 * 1024)).toBe('5.0 MB');
  });
});

describe('uploadAttachments', () => {
  it('uploadAttachments_posts_every_file_in_one_request', async () => {
    const seen: { url?: string; init?: RequestInit; calls: number } = { calls: 0 };
    vi.stubGlobal('fetch', (url: string, init: RequestInit) => {
      seen.url = url;
      seen.init = init;
      seen.calls += 1;
      return Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ attachments: [row(), row({ id: OTHER })] }),
      });
    });

    const uploaded = await uploadAttachments('izzie', [
      new File(['a,b\n1,2\n'], 'data.csv', { type: 'text/csv' }),
      new File(['x'], 'shot.png', { type: 'image/png' }),
    ]);

    // One request, not one per file — dropping two files is one gesture.
    expect(seen.calls).toBe(1);
    expect(uploaded.map((a) => a.id)).toEqual([ID, OTHER]);
    expect(seen.url).toContain('/api/agents/izzie/sessions/persona-izzie/attachments');
    expect(seen.init?.method).toBe('POST');
    const body = seen.init?.body as FormData;
    expect(body).toBeInstanceOf(FormData);
    expect(body.getAll('file')).toHaveLength(2);
  });

  it('uploadAttachments_throws_the_servers_message', async () => {
    vi.stubGlobal('fetch', () =>
      Promise.resolve({
        ok: false,
        status: 413,
        json: () => Promise.resolve({ error: 'attachment `big.bin` is too large' }),
      }),
    );
    await expect(
      uploadAttachments('izzie', [new File(['x'], 'big.bin')]),
    ).rejects.toThrow('attachment `big.bin` is too large');
  });

  it('uploadAttachments_returns_empty_when_the_body_has_no_rows', async () => {
    vi.stubGlobal('fetch', () => Promise.resolve({ ok: true, json: () => Promise.resolve({}) }));
    expect(await uploadAttachments('izzie', [new File(['x'], 'a.txt')])).toEqual([]);
  });
});

describe('fetchSessionAttachments', () => {
  it('fetchSessionAttachments_indexes_rows_by_id', async () => {
    vi.stubGlobal('fetch', () =>
      Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ attachments: [row(), row({ id: OTHER })] }),
      }),
    );
    const index = await fetchSessionAttachments('izzie');
    expect([...index.keys()]).toEqual([ID, OTHER]);
    expect(index.get(ID)?.file_name).toBe('data.csv');
  });

  it('fetchSessionAttachments_is_empty_when_unavailable', async () => {
    vi.stubGlobal('fetch', () => Promise.reject(new Error('down')));
    expect((await fetchSessionAttachments('izzie')).size).toBe(0);
  });
});
