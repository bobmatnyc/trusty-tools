import { tmApi } from '../stores/app';
import type { ListenerFilter } from './listeners';
export interface ChannelBinding { id:string; name:string; provider:'slack'|'telegram'; target:string; enabled:boolean; send_enabled:boolean; receive_enabled:boolean; filter:ListenerFilter; instructions:string; }
export interface ChannelProvider {id:'slack'|'telegram';name:string;configured:boolean;can_send:boolean;can_read:boolean;can_receive?:boolean;}
export interface ChannelConfiguration {agent:string;revision:string;bindings:ChannelBinding[];providers:ChannelProvider[];}
export interface ChannelMessages {available:boolean;messages:{id:string;text:string;from?:string;timestamp?:string|number}[];reason?:string;}
const base=(agent:string)=>`/api/agents/${encodeURIComponent(agent)}/channels`;
export const fetchChannels=(agent:string)=>tmApi<ChannelConfiguration>(base(agent));
export const saveChannels=(agent:string,revision:string,bindings:ChannelBinding[])=>tmApi<ChannelConfiguration>(base(agent),{method:'PUT',body:JSON.stringify({revision,bindings})});
export const fetchChannelMessages=(agent:string,id:string)=>tmApi<ChannelMessages>(`${base(agent)}/${encodeURIComponent(id)}/messages`);
export const sendChannelMessage=(agent:string,id:string,text:string,revision:string)=>tmApi<{ok:boolean;message_id?:string}>(`${base(agent)}/${encodeURIComponent(id)}/send`,{method:'POST',body:JSON.stringify({text,revision})});
