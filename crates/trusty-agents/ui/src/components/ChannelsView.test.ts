import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
import { writable } from 'svelte/store';
vi.mock('../stores/app', () => ({ activeAgentId:writable<string|null>('alice'),agentRoster:writable([{id:'alice',label:'Alice'},{id:'bob',label:'Bob'}]) }));
vi.mock('../lib/channels',()=>({fetchChannels:vi.fn(),saveChannels:vi.fn(),fetchChannelMessages:vi.fn(),sendChannelMessage:vi.fn()}));
vi.mock('../lib/listeners',()=>({fetchListeners:vi.fn(),saveListeners:vi.fn()}));
import ChannelsView from './ChannelsView.svelte';
import { activeAgentId } from '../stores/app';
import { fetchChannels,saveChannels,fetchChannelMessages,sendChannelMessage,type ChannelConfiguration } from '../lib/channels';
import { fetchListeners } from '../lib/listeners';
const filter={from:[],include_labels:[],exclude_labels:[],subject_contains:[],snippet_contains:[]};
// #7427: telegram reports can_receive true and the listing carries gworkspace,
// both matching the adapter registry.
const config=(agent='alice'):ChannelConfiguration=>({agent,revision:'r1',providers:[{id:'slack',name:'Slack',configured:true,can_send:true,can_read:true,can_receive:true,receive_reason:'Automatic updates require the Slack bot listener to be running'},{id:'telegram',name:'Telegram',configured:true,can_send:true,can_read:false,can_receive:true,receive_reason:'Automatic updates require the Telegram long-poll gateway to be running'},{id:'gworkspace',name:'Google Workspace (Gmail)',configured:true,can_send:true,can_read:false,can_receive:true,receive_reason:'Automatic updates require a Gmail listener polling the bound mailbox'}],bindings:[{id:`${agent}-channel`,name:`${agent} updates`,provider:'slack',target:'C123',enabled:true,send_enabled:true,receive_enabled:true,filter,instructions:''}]});
let view:ReturnType<typeof mount>|undefined;
async function settle(){await Promise.resolve();await tick();await Promise.resolve();await tick();}
function button(text:string){return [...document.querySelectorAll('button')].find(b=>b.textContent?.includes(text))!;}
function input(selector:string,text:string){const el=document.querySelector(selector) as HTMLInputElement;el.value=text;el.dispatchEvent(new Event('input',{bubbles:true}));}
async function render(){view=mount(ChannelsView,{target:document.body});await settle();}
beforeEach(()=>{activeAgentId.set('alice');vi.mocked(fetchChannels).mockImplementation(async agent=>config(agent));vi.mocked(fetchChannelMessages).mockResolvedValue({available:true,messages:[]});vi.mocked(sendChannelMessage).mockResolvedValue({ok:true});vi.mocked(fetchListeners).mockResolvedValue({agent:'alice',revision:'l1',listeners:[{name:'custom-source',enabled:true,event_types:[],filter,instructions:''}],available_listeners:[{name:'custom-source',connector:'custom',identity:null,enabled:true}]});});
afterEach(async()=>{if(view)await unmount(view);view=undefined;document.body.innerHTML='';vi.resetAllMocks();});
it('ignores old assistant results after a clean assistant switch',async()=>{
 let finish!:(v:ChannelConfiguration)=>void;vi.mocked(fetchChannels).mockReturnValueOnce(new Promise(resolve=>finish=resolve));await render();activeAgentId.set('bob');await settle();finish(config('alice'));await settle();expect((document.querySelector('[aria-label="Channel name"]') as HTMLInputElement).value).toBe('bob updates');expect(document.body.textContent).not.toContain('alice updates');
});
it('uses the returned revision for subsequent saves',async()=>{
 await render();vi.mocked(saveChannels).mockImplementation(async(agent,_revision,bindings)=>({...config(agent),revision:'r2',bindings:structuredClone(bindings)}));input('[aria-label="Channel name"]','First edit');await settle();button('Save channels').click();await settle();expect(saveChannels).toHaveBeenLastCalledWith('alice','r1',expect.any(Array));input('[aria-label="Channel name"]','Second edit');await settle();button('Save channels').click();await settle();expect(saveChannels).toHaveBeenLastCalledWith('alice','r2',expect.any(Array));
});
it('sends only explicitly through the saved binding and blocks dirty or disabled bindings',async()=>{
 await render();expect(sendChannelMessage).not.toHaveBeenCalled();input('[aria-label="Channel message"]','Hello');await settle();(document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).click();await settle();expect(sendChannelMessage).toHaveBeenCalledWith('alice','alice-channel','Hello','r1');input('[aria-label="Channel name"]','unsaved');input('[aria-label="Channel message"]','Should not send');await settle();const send=document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement;expect(send.disabled).toBe(true);send.click();expect(sendChannelMessage).toHaveBeenCalledTimes(1);
});
it('disables unsupported receiving and history, without inventing update jobs',async()=>{
 const c=config();c.bindings[0]={...c.bindings[0],provider:'telegram',receive_enabled:false};c.providers[1].can_receive=false;vi.mocked(fetchChannels).mockResolvedValue(c);await render();const receive=[...document.querySelectorAll('label')].find(label=>label.textContent?.trim()==='Receive updates')!.querySelector('input') as HTMLInputElement;expect(receive.disabled).toBe(true);expect(button('Refresh messages').disabled).toBe(true);expect(document.body.textContent).toContain('does not provide message history');expect(fetchChannelMessages).not.toHaveBeenCalled();expect(document.body.textContent).not.toMatch(/daily heartbeat|hourly wake/i);
});
it('retains channel drafts and pins requests to their assistant during an external switch',async()=>{
 await render();input('[aria-label="Channel name"]','draft for Alice');await settle();activeAgentId.set('bob');await settle();expect((document.querySelector('[aria-label="Channel name"]') as HTMLInputElement).value).toBe('draft for Alice');expect((document.querySelector('[aria-label="Channel assistant"]') as HTMLSelectElement).disabled).toBe(true);expect(fetchChannels).toHaveBeenCalledTimes(1);vi.mocked(saveChannels).mockImplementation(async(agent,_revision,bindings)=>({...config(agent),bindings,revision:'r2'}));button('Save channels').click();await settle();expect(saveChannels).toHaveBeenLastCalledWith('alice','r1',expect.any(Array));expect(fetchChannels).toHaveBeenLastCalledWith('bob');
});
it('keeps listener edits mounted when hidden and pins the assistant while they are dirty',async()=>{
 await render();button('Configure other update sources').click();await settle();const field=document.querySelector('[aria-label="custom-source instructions"]') as HTMLTextAreaElement;field.value='preserve this';field.dispatchEvent(new Event('input'));await settle();button('Hide other update sources').click();await settle();expect(document.querySelector('[aria-label="custom-source instructions"]')).toBe(field);activeAgentId.set('bob');await settle();expect(fetchChannels).toHaveBeenCalledTimes(1);expect(field.value).toBe('preserve this');expect((document.querySelector('[aria-label="Channel assistant"]') as HTMLSelectElement).disabled).toBe(true);button('Configure other update sources').click();await settle();expect(fetchListeners).toHaveBeenCalledTimes(1);
});
it('preserves unsaved edits on conflict and never sends them',async()=>{
 await render();vi.mocked(saveChannels).mockRejectedValue(new Error('409 conflict'));input('[aria-label="Channel name"]','draft');await settle();button('Save channels').click();await settle();expect((document.querySelector('[aria-label="Channel name"]') as HTMLInputElement).value).toBe('draft');expect(document.querySelector('[role="alert"]')?.textContent).toContain('Your edits are still shown');expect(sendChannelMessage).not.toHaveBeenCalled();
});
it('does not send through a saved disabled binding or a provider without send capability',async()=>{
 const c=config();c.bindings[0].enabled=false;c.providers[0].can_send=false;vi.mocked(fetchChannels).mockResolvedValue(c);await render();input('[aria-label="Channel message"]','blocked');await settle();const send=document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement;expect(send.disabled).toBe(true);send.click();await settle();expect(sendChannelMessage).not.toHaveBeenCalled();
});
it('ignores an old inbox response after switching assistants',async()=>{
 let finish!:(v:Awaited<ReturnType<typeof fetchChannelMessages>>)=>void;vi.mocked(fetchChannelMessages).mockReturnValueOnce(new Promise(resolve=>finish=resolve));await render();button('Refresh messages').click();await settle();activeAgentId.set('bob');await settle();finish({available:true,messages:[{id:'old',text:'private Alice message'}]});await settle();expect(document.body.textContent).not.toContain('private Alice message');expect((document.querySelector('[aria-label="Channel name"]') as HTMLInputElement).value).toBe('bob updates');
});

it('retains the composed message when the saved destination revision conflicts',async()=>{
 await render();vi.mocked(sendChannelMessage).mockRejectedValue(new Error('409 conflict'));input('[aria-label="Channel message"]','Do not lose this message');await settle();(document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).click();await settle();expect(sendChannelMessage).toHaveBeenCalledWith('alice','alice-channel','Do not lose this message','r1');expect((document.querySelector('[aria-label="Channel message"]') as HTMLTextAreaElement).value).toBe('Do not lose this message');expect(document.querySelector('[role="alert"]')?.textContent).toContain('has not been sent');expect(fetchChannelMessages).not.toHaveBeenCalled();vi.mocked(fetchChannels).mockResolvedValue({...config(),revision:'r2'});button('Reload').click();await settle();expect((document.querySelector('[aria-label="Channel message"]') as HTMLTextAreaElement).value).toBe('Do not lose this message');vi.mocked(sendChannelMessage).mockResolvedValue({ok:true});(document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).click();await settle();expect(sendChannelMessage).toHaveBeenLastCalledWith('alice','alice-channel','Do not lose this message','r2');
});
it('keeps a second binding selected when reloading a conflicted unsent message',async()=>{
 const c=config();c.bindings.push({...c.bindings[0],id:'second',name:'Second',target:'C456'});vi.mocked(fetchChannels).mockResolvedValue(c);await render();const select=document.querySelector('[aria-label="Selected channel"]') as HTMLSelectElement;select.value='second';select.dispatchEvent(new Event('change'));await settle();input('[aria-label="Channel message"]','For second only');await settle();vi.mocked(sendChannelMessage).mockRejectedValueOnce(new Error('409'));(document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).click();await settle();vi.mocked(fetchChannels).mockResolvedValue({...c,revision:'r2'});button('Reload').click();await settle();expect((document.querySelector('[aria-label="Selected channel"]') as HTMLSelectElement).value).toBe('second');(document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).click();await settle();expect(sendChannelMessage).toHaveBeenLastCalledWith('alice','second','For second only','r2');
});
it('requires explicit destination selection if the drafted binding is removed during reload',async()=>{
 const c=config();c.bindings.push({...c.bindings[0],id:'second',name:'Second',target:'C456'});vi.mocked(fetchChannels).mockResolvedValue(c);await render();const select=document.querySelector('[aria-label="Selected channel"]') as HTMLSelectElement;select.value='second';select.dispatchEvent(new Event('change'));await settle();input('[aria-label="Channel message"]','For removed second');await settle();vi.mocked(fetchChannels).mockResolvedValue({...config(),revision:'r2'});button('Reload').click();await settle();expect((document.querySelector('[aria-label="Selected channel"]') as HTMLSelectElement).value).toBe('');expect((document.querySelector('[aria-label="Channel message"]') as HTMLTextAreaElement).value).toBe('For removed second');expect((document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).disabled).toBe(true);expect(document.body.textContent).toContain('previous destination is no longer available');expect(sendChannelMessage).not.toHaveBeenCalled();
});
// #7427: pre-change the Receive updates checkbox was disabled for every
// Telegram binding (the provider reported can_receive false), and the
// per-binding dispatch-failure counter had nowhere to render.
it('lets a Telegram binding enable receive and shows its dispatch failures',async()=>{
 const c=config();c.bindings[0]={...c.bindings[0],provider:'telegram',target:'123456',receive_enabled:true};c.bindings[0].credential_ref='telegram';c.status={'alice-channel':{dispatch_failures:3,last_error:'persona dispatch exploded'}};vi.mocked(fetchChannels).mockResolvedValue(c);await render();const receive=[...document.querySelectorAll('label')].find(label=>label.textContent?.trim()==='Receive updates')!.querySelector('input') as HTMLInputElement;expect(receive.disabled).toBe(false);expect(receive.checked).toBe(true);expect(document.body.textContent).toContain('Telegram long-poll gateway');expect(document.body.textContent).toContain('3 incoming messages could not reach the assistant');expect(document.body.textContent).toContain('persona dispatch exploded');
});
it('shows no dispatch-failure notice for a healthy binding',async()=>{
 await render();expect(document.body.textContent).not.toContain('could not reach the assistant');
});
// #7427: pre-change the Service dropdown had no gworkspace option and the
// destination placeholder was a Slack-or-Telegram ternary, so a Gmail binding
// could not be configured or hinted at.
it('offers gworkspace with its own destination placeholder',async()=>{
 const c=config();c.bindings[0]={...c.bindings[0],provider:'gworkspace',target:'from:alice@example.com',receive_enabled:true};vi.mocked(fetchChannels).mockResolvedValue(c);await render();
 const services=[...document.querySelectorAll('select')].flatMap(s=>[...s.options].map(o=>o.value));expect(services).toContain('gworkspace');
 const target=[...document.querySelectorAll('input')].find(i=>i.placeholder.includes('from:'))!;expect(target.placeholder).toBe('from:someone@example.com or label:INBOX');expect(target.value).toBe('from:alice@example.com');
 expect(document.body.textContent).toContain('Gmail listener polling the bound mailbox');
 const receive=[...document.querySelectorAll('label')].find(label=>label.textContent?.trim()==='Receive updates')!.querySelector('input') as HTMLInputElement;expect(receive.disabled).toBe(false);
});
it('keeps a draft visible and editable when reload removes every binding',async()=>{
 await render();input('[aria-label="Channel message"]','Keep this draft');await settle();vi.mocked(fetchChannels).mockResolvedValue({...config(),revision:'r2',bindings:[]});button('Reload').click();await settle();const composer=document.querySelector('[aria-label="Channel message"]') as HTMLTextAreaElement;expect(composer.value).toBe('Keep this draft');expect(composer.disabled).toBe(false);expect((document.querySelector('[aria-label="Send channel message"]') as HTMLButtonElement).disabled).toBe(true);
});
