import DOMPurify from 'dompurify';
import { prepareAttachment } from './prepareAttachments';
import { ATTACHMENT_LIMITS as limits, type ChatAttachment, type DraftAttachment, type TableFormat } from './chatAttachments';
export type ClipboardInput = { file: File } | { name: string; format: TableFormat; text: string };

/** Return null to preserve the textarea's ordinary text paste, including code. */
export function clipboardInputs(data: DataTransfer): ClipboardInput[] | null {
  const files = Array.from(data.files ?? []);
  if (files.length) return files.map(file => ({ file }));
  const html = data.getData('text/html');
  if (html && /<table[\s>]/i.test(html)) {
    if (new TextEncoder().encode(html).length > limits.fileBytes) throw new Error('Pasted table exceeds 5 MiB.');
    const clean = DOMPurify.sanitize(html, { ALLOWED_TAGS: ['table','thead','tbody','tfoot','tr','th','td','br'], ALLOWED_ATTR: ['colspan','rowspan'], KEEP_CONTENT: true });
    return [{ name: 'Pasted table', format: 'clipboard-html', text: clean }];
  }
  const text = data.getData('text/plain');
  const lines = text.replace(/\r\n/g, '\n').replace(/\n$/, '').split('\n');
  const code = lines.some(line => /^\s*(?:const|let|var|return|function|class|import|export|if|for|while|def|print)\b/.test(line) || /[{};]|=>/.test(line));
  if (lines.every(line => line.includes('\t')) && !code) return [{ name: 'Pasted cells', format: 'clipboard-tsv', text }];
  return null;
}

export function parseTableInWorker(name: string, format: TableFormat, data: string | ArrayBuffer): Promise<ChatAttachment> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./attachmentWorker.ts', import.meta.url), { type: 'module' });
    const finish = () => { clearTimeout(timer); worker.terminate(); };
    const timer = setTimeout(() => { finish(); reject(new Error('Table processing exceeded five seconds. Use a smaller range or file.')); }, limits.workerMs);
    worker.onerror = () => { finish(); reject(new Error('Table processing failed. Check the file format.')); };
    worker.onmessage = event => { finish(); event.data.error ? reject(new Error(event.data.error)) : resolve(event.data.attachment); };
    worker.postMessage({ name, format, data }, typeof data === 'string' ? [] : [data]);
  });
}

async function imageAttachment(file: File, bytes: Uint8Array): Promise<ChatAttachment> {
  const jpeg = bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255;
  const png = bytes.slice(0, 8).join(',') === '137,80,78,71,13,10,26,10';
  const webp = new TextDecoder().decode(bytes.slice(0, 4)) === 'RIFF' && new TextDecoder().decode(bytes.slice(8, 12)) === 'WEBP';
  const mime = png ? 'image/png' : jpeg ? 'image/jpeg' : webp ? 'image/webp' : null;
  if (!mime || (file.type && file.type !== mime)) throw new Error('Only valid PNG, JPEG and WebP images are supported.');
  const url = URL.createObjectURL(file);
  try {
    await new Promise<void>((resolve, reject) => {
      const image = new Image();
      const timer = setTimeout(() => { image.src = ''; reject(new Error('Image could not be decoded in time.')); }, limits.workerMs);
      image.onerror = () => { clearTimeout(timer); reject(new Error('Image could not be decoded.')); };
      image.onload = () => { clearTimeout(timer); image.naturalWidth > limits.imageSide || image.naturalHeight > limits.imageSide || image.naturalWidth * image.naturalHeight > limits.imagePixels ? reject(new Error('Image exceeds 8192 pixels per side or 24 megapixels.')) : resolve(); };
      image.src = url;
    });
  } finally { URL.revokeObjectURL(url); }
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  return { kind: 'image', name: file.name.slice(0, 255) || 'Pasted image', mime_type: mime, data_base64: btoa(binary) };
}

export async function parseClipboardInput(input: ClipboardInput): Promise<DraftAttachment> {
  let attachment: ChatAttachment, sourceBytes: number;
  if ('file' in input) {
    const file = input.file;
    sourceBytes = file.size;
    if (sourceBytes > limits.fileBytes) throw new Error(`${file.name} exceeds 5 MiB.`);
    const data = await file.arrayBuffer();
    if (file.type.startsWith('image/') || /\.(png|jpe?g|webp)$/i.test(file.name)) attachment = await imageAttachment(file, new Uint8Array(data));
    else if (/\.xlsx$/i.test(file.name)) attachment = await parseTableInWorker(file.name, 'xlsx', data);
    else if (/\.csv$/i.test(file.name) || file.type === 'text/csv') attachment = await parseTableInWorker(file.name, 'csv', new TextDecoder('utf-8', { fatal: true }).decode(data));
    else throw new Error(`${file.name || 'Clipboard file'} is unsupported. Paste PNG, JPEG, WebP, CSV or XLSX.`);
  } else {
    sourceBytes = new TextEncoder().encode(input.text).length;
    if (sourceBytes > limits.fileBytes) throw new Error('Pasted table exceeds 5 MiB.');
    attachment = await parseTableInWorker(input.name, input.format, input.text);
  }
  // #7370: local libraries validate previews; only API-normalized content reaches submit.
  attachment = await prepareAttachment(input, attachment);
  return { id: crypto.randomUUID(), sourceBytes, attachment };
}
