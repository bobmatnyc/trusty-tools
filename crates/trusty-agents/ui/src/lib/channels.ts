import { tmApi } from '../stores/app';
import { apiBase } from './api-config';
import type { ListenerFilter } from './listeners';
// #7427: `credential_ref` names the credential a binding sends as (never a
// value; the server resolves the name at send time), and `status` carries the
// per-binding dispatch-failure counter so a binding dropping inbound wakes says
// so instead of reading as healthy. gworkspace (Gmail) joins Slack and Telegram
// as a two-way channel; its target is a correspondent (`from:someone@example.com`)
// or a label (`label:INBOX`), not a destination id, so the placeholder is per
// provider.
export type ChannelProviderId='slack'|'telegram'|'gworkspace';
export interface ChannelBinding { id:string; name:string; provider:ChannelProviderId; target:string; enabled:boolean; send_enabled:boolean; receive_enabled:boolean; filter:ListenerFilter; instructions:string; credential_ref?:string; }
export interface ChannelProvider {id:ChannelProviderId;name:string;configured:boolean;can_send:boolean;can_read:boolean;can_receive?:boolean;receive_reason?:string;}
export interface ChannelBindingStatus {dispatch_failures:number;last_error:string|null;}
export interface ChannelConfiguration {agent:string;revision:string;bindings:ChannelBinding[];providers:ChannelProvider[];status?:Record<string,ChannelBindingStatus>;}
export interface ChannelMessages {available:boolean;messages:{id:string;text:string;from?:string;timestamp?:string|number}[];reason?:string;}
const base=(agent:string)=>`/api/agents/${encodeURIComponent(agent)}/channels`;
export const fetchChannels=(agent:string)=>tmApi<ChannelConfiguration>(base(agent));
// #7609: a channel write is the one operation this API gates on a bearer
// credential even from loopback, because whoever can write a binding can
// redirect every assistant's inbound traffic. On a daemon started without
// `--api-token` the credential is minted per boot and published on the
// unauthenticated `/api/config` probe, to a same-origin caller only — so the
// UI the daemon serves can still save, while a page from anywhere else cannot
// read the value (the CORS layer is the same-origin variant, so it never gets
// the response body). Cached for the page's lifetime: it is stable for the
// daemon's boot and NOT persisted, because the next boot mints a new one.
let channelWriteToken: string | null = null;
async function channelWriteAuth(): Promise<Record<string, string>> {
  if (channelWriteToken === null) {
    try {
      const r = await fetch(`${apiBase()}/api/config`);
      const cfg = r.ok ? ((await r.json()) as { channel_write_token?: string }) : {};
      channelWriteToken = typeof cfg.channel_write_token === 'string' ? cfg.channel_write_token : '';
    } catch {
      // A probe that failed is not a credential we may invent; the save below
      // will surface the server's own 401.
      channelWriteToken = '';
    }
  }
  return channelWriteToken ? { Authorization: `Bearer ${channelWriteToken}` } : {};
}
/** Forget the cached credential, so the next save re-probes. */
export function resetChannelWriteToken(): void { channelWriteToken = null; }
export const saveChannels=async(agent:string,revision:string,bindings:ChannelBinding[])=>tmApi<ChannelConfiguration>(base(agent),{method:'PUT',headers:await channelWriteAuth(),body:JSON.stringify({revision,bindings})});
export const fetchChannelMessages=(agent:string,id:string)=>tmApi<ChannelMessages>(`${base(agent)}/${encodeURIComponent(id)}/messages`);
export const sendChannelMessage=(agent:string,id:string,text:string,revision:string)=>tmApi<{ok:boolean;message_id?:string}>(`${base(agent)}/${encodeURIComponent(id)}/send`,{method:'POST',body:JSON.stringify({text,revision})});
