// #7370: spreadsheet parsing belongs to SheetJS; ZIP limits are checked before it expands XLSX.
import * as XLSX from 'xlsx';
import { Unzip, UnzipInflate, zipSync } from 'fflate';
import { ATTACHMENT_LIMITS as limits, type ChatAttachment, type TableFormat } from './chatAttachments';

export function checkWorkbookExpansion(bytes: Uint8Array): Uint8Array {
  const files: Record<string, Uint8Array> = Object.create(null);
  const names = new Set<string>();
  let expanded = 0, entries = 0, completed = 0;
  const unzip = new Unzip(file => {
    if (names.has(file.name) || file.name.length > 1024) throw new Error('Workbook archive has duplicate or invalid entries.');
    names.add(file.name);
    const chunks: Uint8Array[] = [];
    let size = 0;
    if (++entries > 256 || (file.originalSize ?? 0) > limits.expandedBytes) throw new Error('Workbook expands beyond the supported limit.');
    file.ondata = (error, data, final) => {
      if (error) throw error;
      expanded += data.byteLength;
      if (expanded > limits.expandedBytes) throw new Error('Workbook expands beyond 16 MiB.');
      chunks.push(data); size += data.byteLength;
      if (final) {
        const joined = new Uint8Array(size); let offset = 0;
        for (const chunk of chunks) { joined.set(chunk, offset); offset += chunk.byteLength; }
        files[file.name] = joined; completed++;
      }
    };
    file.start();
  });
  unzip.register(UnzipInflate);
  // Small compressed chunks bound allocations between expansion checks.
  for (let offset = 0; offset < bytes.length; offset += 1024) unzip.push(bytes.subarray(offset, offset + 1024), offset + 1024 >= bytes.length);
  if (!entries || completed !== entries) throw new Error('Workbook archive is incomplete.');
  // Parse a canonical ZIP so a conflicting central directory cannot bypass the streaming guard.
  return zipSync(files, { level: 0 });
}

export function parseTable(name: string, format: TableFormat, data: string | ArrayBuffer): ChatAttachment {
  const size = typeof data === 'string' ? new TextEncoder().encode(data).length : data.byteLength;
  if (size > limits.fileBytes) throw new Error('Table input exceeds 5 MiB.');
  if (format === 'xlsx') {
    if (typeof data === 'string') throw new Error('Workbook bytes are required.');
    data = checkWorkbookExpansion(new Uint8Array(data)).buffer as ArrayBuffer;
  }
  const book = XLSX.read(data, { type: typeof data === 'string' ? 'string' : 'array', raw: format !== 'xlsx', cellFormula: true, cellText: true, sheetRows: limits.rows + 1, ...(format === 'clipboard-tsv' ? { FS: '\t' } : {}) });
  if (!book.SheetNames.length || book.SheetNames.length > limits.sheets) throw new Error('A table attachment supports one to three sheets.');
  let characters = 0;
  const sheets = book.SheetNames.map(sheetName => {
    const sheet = book.Sheets[sheetName];
    const range = XLSX.utils.decode_range(sheet['!fullref'] || sheet['!ref'] || 'A1');
    if (range.e.r >= limits.rows || range.e.c >= limits.columns) throw new Error('Tables support at most 200 rows and 30 columns per sheet.');
    for (const [address, cell] of Object.entries(sheet)) {
      if (!address.startsWith('!') && cell.f && cell.v == null) { cell.t = 's'; cell.v = '[formula value unavailable]'; delete cell.w; }
    }
    const rows = XLSX.utils.sheet_to_json<unknown[]>(sheet, { header: 1, raw: false, defval: '', blankrows: true, range: { s: { r: 0, c: 0 }, e: range.e } })
      .map(row => Array.from({ length: range.e.c + 1 }, (_, column) => String(row[column] ?? '')));
    characters += rows.reduce((sum, row) => sum + row.reduce((n, cell) => n + cell.length, 0), 0);
    if (characters > limits.characters) throw new Error('Tables exceed 50,000 characters.');
    return { name: sheetName.slice(0, 255), rows };
  });
  return { kind: 'table', name: name.slice(0, 255), source_format: format, sheets };
}
