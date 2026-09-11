import { tmApi } from '../stores/app';
import type { ListenerFilter } from './listeners';
// #7427: `credential_ref` names the credential a binding sends as (never a
// value; the server resolves the name at send time), and `status` carries the
// per-binding dispatch-failure counter so a binding dropping inbound wakes says
// so instead of reading as healthy.
export interface ChannelBinding { id:string; name:string; provider:'slack'|'telegram'; target:string; enabled:boolean; send_enabled:boolean; receive_enabled:boolean; filter:ListenerFilter; instructions:string; credential_ref?:string; }
export interface ChannelProvider {id:'slack'|'telegram';name:string;configured:boolean;can_send:boolean;can_read:boolean;can_receive?:boolean;receive_reason?:string;}
export interface ChannelBindingStatus {dispatch_failures:number;last_error:string|null;}
export interface ChannelConfiguration {agent:string;revision:string;bindings:ChannelBinding[];providers:ChannelProvider[];status?:Record<string,ChannelBindingStatus>;}
export interface ChannelMessages {available:boolean;messages:{id:string;text:string;from?:string;timestamp?:string|number}[];reason?:string;}
const base=(agent:string)=>`/api/agents/${encodeURIComponent(agent)}/channels`;
export const fetchChannels=(agent:string)=>tmApi<ChannelConfiguration>(base(agent));
export const saveChannels=(agent:string,revision:string,bindings:ChannelBinding[])=>tmApi<ChannelConfiguration>(base(agent),{method:'PUT',body:JSON.stringify({revision,bindings})});
export const fetchChannelMessages=(agent:string,id:string)=>tmApi<ChannelMessages>(`${base(agent)}/${encodeURIComponent(id)}/messages`);
export const sendChannelMessage=(agent:string,id:string,text:string,revision:string)=>tmApi<{ok:boolean;message_id?:string}>(`${base(agent)}/${encodeURIComponent(id)}/send`,{method:'POST',body:JSON.stringify({text,revision})});
