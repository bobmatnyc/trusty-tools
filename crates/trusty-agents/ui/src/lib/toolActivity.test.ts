import {beforeEach,it,expect} from 'vitest';
import {get} from 'svelte/store';
import {messages,recordToolActivity,replaceMessageTaskId,updateMessageByTask,streamDeltaIntoTask,finishToolActivities} from '../stores/app';
import {historyToMessages} from './chatHistory';
beforeEach(()=>messages.set(new Map([['one',[{id:'reply',role:'assistant',content:'',timestamp:1,taskId:'real'}]],['two',[]]])));
it('routes to task owner and keeps tool rows out of text updates',()=>{
 recordToolActivity({task_id:'real',call_id:'a',tool:'Read',status:'running'});
 updateMessageByTask('one','real','answer');streamDeltaIntoTask('real','streamed');
 const list=get(messages).get('one')!;expect(list[0].role).toBe('tool');expect(list[0].content).toBe('');expect(list[1].content).toBe('streamed');expect(get(messages).get('two')).toEqual([]);
 recordToolActivity({task_id:'real',call_id:'a',tool:'Read',status:'complete'});recordToolActivity({task_id:'real',call_id:'a',tool:'Read',status:'running'});
 expect(get(messages).get('one')![0].activityStatus).toBe('complete');
});
it('replays early events only when matching pending task is reconciled',()=>{
 messages.set(new Map([['one',[{id:'reply',role:'assistant',content:'',timestamp:1,taskId:'pending'}]]]));
 recordToolActivity({task_id:'early-fixture',call_id:'a',tool:'Search',status:'complete'});expect(get(messages).get('one')).toHaveLength(1);
 replaceMessageTaskId('one','pending','early-fixture');expect(get(messages).get('one')![0].role).toBe('tool');
});
it('ends unfinished activity honestly on task cancellation',()=>{
 recordToolActivity({task_id:'real',call_id:'a',tool:'Read',status:'running'});finishToolActivities('real','error');expect(get(messages).get('one')![0].activityStatus).toBe('error');recordToolActivity({task_id:'real',call_id:'a',tool:'Read',status:'running'});expect(get(messages).get('one')![0].activityStatus).toBe('error');
});
it('restores explicit system event and tool metadata but never interprets user text',()=>{
 const event=JSON.stringify({kind:'trusty.listener-event',version:1,listener:'mail',event_type:'message.received',subject:'Invoice'});
 const tool=JSON.stringify({kind:'trusty.tool-activity',version:1,tool:'Read',call_id:'a',status:'complete'});
 const result=historyToMessages({available:true,messages:[{role:'system',content:event},{role:'user',content:event},{role:'system',content:tool}],start:0,total:3,has_more:false,updated_at:null},'A',1);
 expect(result.map(m=>m.role)).toEqual(['event','user','tool']);expect(result[0].content).toContain('Invoice');expect(result[2].toolName).toBe('Read');expect(result[2].content).toBe('');
});
