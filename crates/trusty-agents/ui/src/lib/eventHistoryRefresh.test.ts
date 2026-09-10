import {afterEach,beforeEach,it,expect,vi} from 'vitest';
import {get} from 'svelte/store';
import {messages,conversationKey,isRunning,prependMessages} from '../stores/app';
import {refreshEventHistory,rehydrateChat,historyToMessages} from './chatHistory';
afterEach(()=>vi.unstubAllGlobals());beforeEach(()=>isRunning.set(false));
const event=(id:string)=>({role:'system',content:JSON.stringify({kind:'trusty.listener-event',version:1,event_id:id,listener:'mail',event_type:'received'})});
const reply={role:'assistant',content:'An update arrived.'};
const page=(rows:any[],start=0,total=rows.length)=>({available:true,start,total,has_more:start>0,updated_at:null,messages:rows});
const serve=(body:unknown)=>vi.stubGlobal('fetch',vi.fn(async()=>new Response(JSON.stringify(body),{status:200,headers:{'Content-Type':'application/json'}})));
async function seed(agent:string,rows:any[]=[],start=0,total=rows.length){messages.set(new Map([[conversationKey('p',agent),[]],['other',[]]]));serve(page(rows,start,total));await rehydrateChat(agent,agent,'p');}
it('appends new event rows without replacing concurrently added messages and dedupes notifications',async()=>{
 await seed('refresh-a');let resolve!:(r:Response)=>void;vi.stubGlobal('fetch',vi.fn(()=>new Promise<Response>(r=>resolve=r)));
 const pending=refreshEventHistory('refresh-a','A','p');const key=conversationKey('p','refresh-a');messages.set(new Map([[key,[{id:'draft',role:'user',content:'Keep this',timestamp:1}]],['other',[]]]));
 resolve(new Response(JSON.stringify(page([event('one'),reply])),{status:200}));expect(await pending).toBe(2);expect(get(messages).get(key)![0].content).toBe('Keep this');expect(get(messages).get('other')).toEqual([]);
 serve(page([event('one'),reply]));expect(await refreshEventHistory('refresh-a','A','p')).toBe(0);
});
it('does not append older unloaded events and preserves IDs for earlier-page dedup',async()=>{
 await seed('refresh-b',[{role:'user',content:'recent'},reply],2,4);
 const rows=[event('old'),reply,{role:'user',content:'recent'},reply,event('new'),reply];serve(page(rows));expect(await refreshEventHistory('refresh-b','B','p')).toBe(2);
 const key=conversationKey('p','refresh-b');expect(get(messages).get(key)!.map(m=>m.id)).toEqual(['history-2','history-3','history-4','history-5']);
 prependMessages(key,historyToMessages(page(rows.slice(0,4)),'B',1));expect(get(messages).get(key)).toHaveLength(6);
});
it('completes an event partially observed during initial loading exactly once',async()=>{
 await seed('refresh-c',[event('partial')]);serve(page([event('partial'),reply]));expect(await refreshEventHistory('refresh-c','C','p')).toBe(1);expect(await refreshEventHistory('refresh-c','C','p')).toBe(0);
 expect(get(messages).get(conversationKey('p','refresh-c'))!.map(m=>m.id)).toEqual(['history-0','history-1']);
});
it('keeps high-water unchanged on transient failure or task starting during fetch',async()=>{
 await seed('refresh-d');serve({available:false,reason:'offline',messages:[],start:0,total:0,has_more:false});await expect(refreshEventHistory('refresh-d','D','p')).rejects.toThrow('offline');
 serve(page([event('retry'),reply]));isRunning.set(true);await expect(refreshEventHistory('refresh-d','D','p')).rejects.toThrow('active task');isRunning.set(false);expect(await refreshEventHistory('refresh-d','D','p')).toBe(2);
});

it('retains the marker of a trailing event not yet fully persisted',async()=>{
 await seed('refresh-e');serve(page([event('A'),reply,event('B')]));expect(await refreshEventHistory('refresh-e','E','p')).toBe(2);
 serve(page([event('A'),reply,event('B'),reply]));expect(await refreshEventHistory('refresh-e','E','p')).toBe(2);
 expect(get(messages).get(conversationKey('p','refresh-e'))!.map(m=>m.role)).toEqual(['event','assistant','event','assistant']);
});

it('handles the first incoming event after an explicitly absent session',async()=>{
 const key=conversationKey('p','refresh-first');messages.set(new Map([[key,[]]]));serve({...page([]),available:false,session_absent:true});await rehydrateChat('refresh-first','First','p');
 serve(page([event('first'),reply]));expect(await refreshEventHistory('refresh-first','First','p')).toBe(2);
});

it('advances past an abandoned incomplete event followed by a later turn',async()=>{
 await seed('refresh-orphan');serve(page([event('orphan'),{role:'user',content:'later'},event('complete'),reply]));expect(await refreshEventHistory('refresh-orphan','O','p')).toBe(2);
 serve(page([event('next'),reply],4,6));expect(await refreshEventHistory('refresh-orphan','O','p')).toBe(2);
});

it('recovers an initially unavailable baseline without overwriting live messages',async()=>{
 const key=conversationKey('p','refresh-recovered');messages.set(new Map([[key,[]]]));serve({...page([]),available:false,reason:'offline'});await rehydrateChat('refresh-recovered','Recovered','p');
 messages.set(new Map([[key,[{id:'live',role:'user',content:'Keep my current message',timestamp:1}]]]));
 serve(page([{role:'user',content:'Earlier'},reply,event('recovered'),reply]));expect(await refreshEventHistory('refresh-recovered','Recovered','p')).toBe(4);
 expect(get(messages).get(key)!.map(m=>m.id)).toEqual(['history-0','history-1','history-2','history-3','live']);
 expect(await refreshEventHistory('refresh-recovered','Recovered','p')).toBe(0);
});
