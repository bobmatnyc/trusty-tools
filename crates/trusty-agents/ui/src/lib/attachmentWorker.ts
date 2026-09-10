import { parseTable } from './tableAttachments';
import type { TableFormat } from './chatAttachments';
self.onmessage = (event: MessageEvent<{ name: string; format: TableFormat; data: string | ArrayBuffer }>) => {
  try { self.postMessage({ attachment: parseTable(event.data.name, event.data.format, event.data.data) }); }
  catch (error) { self.postMessage({ error: error instanceof Error ? error.message : String(error) }); }
};
