import { afterEach, expect, it, vi } from 'vitest';
vi.mock('./api-config', () => ({ apiBase: () => '' }));
vi.mock('../stores/app', () => ({ getCurrentApiToken: () => 'fixture-token' }));
import { prepareAttachment } from './prepareAttachments';
const preview = {kind:'table' as const,name:'cells',source_format:'clipboard-tsv' as const,sheets:[{name:'Local',rows:[['A','B']]}]};
afterEach(() => vi.unstubAllGlobals());
it('submits authenticated raw clipboard content and uses canonical server values', async () => {
  const canonical = {...preview,sheets:[{name:'Canonical',rows:[['normalized','B']]}]};
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({attachments:[canonical]})));
  vi.stubGlobal('fetch',fetcher);
  expect(await prepareAttachment({name:'cells',format:'clipboard-tsv',text:'A\tB'},preview)).toEqual(canonical);
  expect(fetcher).toHaveBeenCalledWith('/api/chat-attachments/prepare',expect.objectContaining({headers:{'Content-Type':'application/json',Authorization:'Bearer fixture-token'},body:JSON.stringify({items:[{kind:'clipboard',name:'cells',format:'tsv',text:'A\tB'}]})}));
});
it('sends original file bytes, and surfaces API rejection or malformed success', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({attachments:[preview]}))).mockResolvedValueOnce(new Response(JSON.stringify({error:'Workbook exceeds expanded limit'}),{status:413})).mockResolvedValueOnce(new Response(JSON.stringify({attachments:[{kind:'table'}]})));
  vi.stubGlobal('fetch',fetcher);
  const file = new File(['A,B\n1,2'],'cells.csv',{type:'text/csv'});
  // jsdom's File lacks Blob.arrayBuffer; read the real fixture through its FileReader.
  Object.defineProperty(file,'arrayBuffer',{value:() => new Promise<ArrayBuffer>((resolve,reject) => {
    const reader = new FileReader(); reader.onload = () => resolve(reader.result as ArrayBuffer); reader.onerror = reject; reader.readAsArrayBuffer(file);
  })});
  const input = {file};
  await prepareAttachment(input,preview);
  expect(JSON.parse(fetcher.mock.calls[0][1].body).items[0]).toEqual({kind:'file',name:'cells.csv',mime_type:'text/csv',data_base64:btoa('A,B\n1,2')});
  await expect(prepareAttachment(input,preview)).rejects.toThrow('expanded limit');
  await expect(prepareAttachment(input,preview)).rejects.toThrow('invalid response');
});
