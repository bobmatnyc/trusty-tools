import { describe, expect, it } from 'vitest';
import * as XLSX from 'xlsx';
import { zipSync } from 'fflate';
import { parseTable, checkWorkbookExpansion } from './tableAttachments';
import { clipboardInputs } from './clipboardAttachments';
import { checkAttachmentBatch, type DraftAttachment } from './chatAttachments';

describe('bounded spreadsheet parsing', () => {
  it.each([
    ['csv', 'name,notes,empty\r\nAlice,"one,two\nthree",', [['name','notes','empty'],['Alice','one,two\nthree','']]],
    ['clipboard-tsv', 'a\tb\tc\n\tvalue\t', [['a','b','c'],['','value','']]],
    ['clipboard-html', '<table><tr><td>A</td><td></td></tr><tr><td>B</td><td>C</td></tr></table>', [['A',''],['B','C']]],
  ] as const)('preserves cells and blanks in %s', (format, data, expected) => {
    const result = parseTable('Original document', format, data);
    expect(result.kind === 'table' && result.sheets[0].rows).toEqual(expected);
    expect(result.name).toBe('Original document');
  });
  it('reads workbook sheets and marks formulas without cached values', () => {
    const book = XLSX.utils.book_new();
    const sheet = XLSX.utils.aoa_to_sheet([['001','value'],['',42]]);
    sheet.C1 = { t: 'n', f: '2+2' }; sheet['!ref'] = 'A1:C2';
    XLSX.utils.book_append_sheet(book, sheet, 'Budget');
    XLSX.utils.book_append_sheet(book, XLSX.utils.aoa_to_sheet([['Second']]), 'Notes');
    const parsed = parseTable('Budget.xlsx', 'xlsx', XLSX.write(book, { type: 'array', bookType: 'xlsx', compression: true }));
    expect(parsed.kind === 'table' && parsed.sheets.map(sheet => sheet.name)).toEqual(['Budget','Notes']);
    expect(parsed.kind === 'table' && parsed.sheets[0].rows).toEqual([['001','value','[formula value unavailable]'],['','42','']]);
  });
  it('rejects excess rows and expanded ZIP data instead of truncating', () => {
    expect(() => parseTable('rows.csv', 'csv', Array.from({length:201}, () => 'x,y').join('\n'))).toThrow('200 rows');
    expect(() => checkWorkbookExpansion(zipSync({ 'large.xml': new Uint8Array(17 * 1024 * 1024) }, { level: 9 }))).toThrow('limit');
    expect(() => parseTable('bad.xlsx', 'xlsx', new Uint8Array([1,2,3]).buffer)).toThrow();
  });
});
it('preserves ordinary text/code paste and sanitizes table HTML without duplicate TSV', () => {
  const transfer = (plain: string, html = '') => ({ files: [], getData: (type: string) => type === 'text/html' ? html : plain }) as unknown as DataTransfer;
  expect(clipboardInputs(transfer('hello\nworld'))).toBeNull();
  expect(clipboardInputs(transfer('\tconst x = 1;\n\treturn x;'))).toBeNull();
  expect(clipboardInputs(transfer('A\tB'))).toEqual([{name:'Pasted cells',format:'clipboard-tsv',text:'A\tB'}]);
  expect(clipboardInputs(transfer('A\t'))).toEqual([{name:'Pasted cells',format:'clipboard-tsv',text:'A\t'}]);
  expect(clipboardInputs(transfer('\tA\n\tB'))).toEqual([{name:'Pasted cells',format:'clipboard-tsv',text:'\tA\n\tB'}]);
  const inputs = clipboardInputs(transfer('A\tB\nC\tD', '<table onclick="bad()"><tr><td>A<script>bad()</script></td></tr></table><img src="https://example.com/a">'));
  expect(inputs).toHaveLength(1);
  expect(inputs && 'text' in inputs[0] && inputs[0].text).not.toMatch(/script|onclick|img|https:/);
});
it('rejects aggregate attachment limits', () => {
  const item: DraftAttachment = {id:'one',sourceBytes:1,attachment:{kind:'table',name:'t',source_format:'csv',sheets:[{name:'S',rows:[['x']]}]}};
  expect(() => checkAttachmentBatch(Array(5).fill(item))).toThrow('four');
  expect(() => checkAttachmentBatch([{...item,sourceBytes:6*1024*1024}])).toThrow('5 MiB');
});
