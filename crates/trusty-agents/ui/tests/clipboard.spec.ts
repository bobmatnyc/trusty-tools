import { test, expect } from '@playwright/test';

// #7370: real browser clipboard event, decoder and bundled spreadsheet worker; backend is intercepted.
test('pastes image and quoted spreadsheet cells into a typed task payload', async ({ page }) => {
  let submitted: Record<string, unknown> | undefined;
  let csvAttempts = 0;
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    const json = (value: unknown, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(value) });
    if (path === '/api/chat-attachments/prepare') {
      const item = route.request().postDataJSON().items[0];
      expect(item.kind).toBe('file');
      if (item.mime_type === 'image/png') return json({attachments:[{kind:'image',name:item.name,mime_type:item.mime_type,data_base64:item.data_base64}]});
      expect(Buffer.from(item.data_base64,'base64').toString()).toBe('name,notes,empty\r\nAlice,"one,two\nthree",');
      if (++csvAttempts === 1) return json({error:'fixture: preparation temporarily unavailable'},503);
      return json({attachments:[{kind:'table',name:item.name,source_format:'csv',sheets:[{name:'Canonical API sheet',rows:[['name','notes','empty'],['Alice','one,two\nthree','']]}]}]});
    }
    if (path === '/api/task' && route.request().method() === 'POST') { submitted = route.request().postDataJSON(); return json({error:'fixture: payload captured, no model invoked'},422); }
    if (path === '/api/health') return json({status:'ok'});
    if (path === '/api/config') return json({auth_required:false});
    if (path === '/api/tasks' || path === '/api/projects') return json([]);
    if (path.endsWith('/chat-history')) return json({available:true,messages:[],start:0,total:0,has_more:false,updated_at:null});
    if (path.includes('models')) return json({providers:[],local:{available:false}});
    if (path.includes('agents')) return json({agents:[]});
    return json({});
  });
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await page.getByRole('tab', {name:'Chat',exact:true}).click();
  const input = page.getByRole('textbox', {name:'Message',exact:true});
  await expect(input).toBeVisible();
  await input.evaluate(async element => {
    const canvas = document.createElement('canvas'); canvas.width = 16; canvas.height = 16;
    const context = canvas.getContext('2d')!; context.fillStyle = 'red'; context.fillRect(0,0,16,16);
    const blob = await new Promise<Blob>(resolve => canvas.toBlob(value => resolve(value!), 'image/png'));
    const data = new DataTransfer(); data.items.add(new File([blob], 'red-square.png', {type:'image/png'}));
    element.dispatchEvent(new ClipboardEvent('paste', {clipboardData:data,bubbles:true,cancelable:true}));
  });
  await expect(page.getByRole('button',{name:'Remove red-square.png',exact:true})).toBeVisible();
  await input.evaluate(element => {
    const data = new DataTransfer();
    data.items.add(new File(['name,notes,empty\r\nAlice,"one,two\nthree",'], 'cells.csv', {type:'text/csv'}));
    element.dispatchEvent(new ClipboardEvent('paste', {clipboardData:data,bubbles:true,cancelable:true}));
  });
  await expect(page.getByRole('alert')).toContainText('preparation temporarily unavailable');
  await expect(page.getByRole('button',{name:'Send message',exact:true})).toBeDisabled();
  await input.evaluate(element => {
    for (const file of [new File(['new,value'],'replacement.csv',{type:'text/csv'}),new File([new Uint8Array(6 * 1024 * 1024)],'oversized.csv',{type:'text/csv'})]) {
      const data = new DataTransfer(); data.items.add(file);
      element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));
    }
  });
  await expect(page.getByRole('alert')).toContainText('Retry or discard the previous failed paste');
  expect(csvAttempts).toBe(1);
  await page.getByRole('button',{name:'Retry pasted files',exact:true}).click();
  await expect(page.getByRole('button',{name:'Remove cells.csv',exact:true})).toBeVisible();
  await page.getByRole('button',{name:'Send message',exact:true}).click();
  await expect.poll(() => submitted).toBeTruthy();
  const attachments = submitted!.attachments as Array<Record<string, any>>;
  expect(attachments[0]).toMatchObject({kind:'image',name:'red-square.png',mime_type:'image/png'});
  expect(attachments[0].data_base64).toMatch(/^iVBOR/);
  expect(attachments[1].sheets[0].rows).toEqual([['name','notes','empty'],['Alice','one,two\nthree','']]);
  expect(attachments[1].sheets[0].name).toBe('Canonical API sheet');
  await expect(page.getByRole('button',{name:'Restore failed message',exact:true})).toBeVisible();
});
