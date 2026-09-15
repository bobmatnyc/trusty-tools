// Both channel scopes, one transport module (#7609 slice 6).
//
// Why: the daemon serves the SAME interaction twice — a revision, a complete
// list, a compare-and-swap on write — once per assistant
// (`/api/agents/{name}/channels`) and once harness-wide (`/api/channels`). One
// module for both keeps the credential handling, the error mapping and the
// request shape from drifting between the two views that call them.
//
// What: the per-assistant half is unchanged. The global half adds
// `fetchGlobalChannels`/`saveGlobalChannels`; the PUT body is exactly
// `{revision, channels}` because `GlobalUpdate` is `deny_unknown_fields` and
// echoing back the `providers`/`scope` the GET carries is a 422. The filter
// type moved here from the deleted `listeners.ts`, which had no other reason
// to exist once the deprecated Listeners editor went.
// Test: `channel-auth.test.ts` (the credential on the wire, both scopes),
// `channels.global.test.ts` (the global request shape and error mapping),
// `ChannelsView.test.ts`, `GlobalChannelsPanel.test.ts`.
import { tmApi } from '../stores/app';
import { withChannelWriteAuth } from './channel-auth';
/** Rust `AgentBindingFilter`: which fetched events wake an assistant. */
export interface ChannelFilter { from:string[]; include_labels:string[]; exclude_labels:string[]; subject_contains:string[]; snippet_contains:string[]; }
/** Rust `ListenerFilter`: what is fetched from the provider at all. */
export interface ChannelIngestFilter { label_ids:string[]; }
// #7427: `credential_ref` names the credential a binding sends as (never a
// value; the server resolves the name at send time), and `status` carries the
// per-binding dispatch-failure counter so a binding dropping inbound wakes says
// so instead of reading as healthy. gworkspace (Gmail) joins Slack and Telegram
// as a two-way channel; its target is a correspondent (`from:someone@example.com`)
// or a label (`label:INBOX`), not a destination id, so the placeholder is per
// provider.
export type ChannelProviderId='slack'|'telegram'|'gworkspace';
export interface ChannelBinding { id:string; name:string; provider:ChannelProviderId; target:string; enabled:boolean; send_enabled:boolean; receive_enabled:boolean; filter:ChannelFilter; instructions:string; credential_ref?:string; }
export interface ChannelProvider {id:ChannelProviderId;name:string;configured:boolean;can_send:boolean;can_read:boolean;can_receive?:boolean;receive_reason?:string;}
export interface ChannelBindingStatus {dispatch_failures:number;last_error:string|null;}
export interface ChannelConfiguration {agent:string;revision:string;bindings:ChannelBinding[];providers:ChannelProvider[];status?:Record<string,ChannelBindingStatus>;}
export interface ChannelMessages {available:boolean;messages:{id:string;text:string;from?:string;timestamp?:string|number}[];reason?:string;}
const base=(agent:string)=>`/api/agents/${encodeURIComponent(agent)}/channels`;
export const fetchChannels=(agent:string)=>tmApi<ChannelConfiguration>(base(agent));
// #7609: a channel write carries the credential from `channel-auth`; see
// that module for why it exists and how it is obtained.
export const saveChannels=(agent:string,revision:string,bindings:ChannelBinding[])=>withChannelWriteAuth(headers=>tmApi<ChannelConfiguration>(base(agent),{method:'PUT',headers,body:JSON.stringify({revision,bindings})}));
export const fetchChannelMessages=(agent:string,id:string)=>tmApi<ChannelMessages>(`${base(agent)}/${encodeURIComponent(id)}/messages`);
export const sendChannelMessage=(agent:string,id:string,text:string,revision:string)=>tmApi<{ok:boolean;message_id?:string}>(`${base(agent)}/${encodeURIComponent(id)}/send`,{method:'POST',body:JSON.stringify({text,revision})});

/**
 * One harness-wide channel, mirroring Rust `channels::Channel` field for field.
 *
 * Why: the UI edits six of these fields and must hand the other seven back
 * untouched. Typing the whole record — rather than the subset the form shows —
 * is what lets a save round-trip `transport`, `poll_interval_secs`, the two
 * filters and the event types the operator (or the migration) set, instead of
 * silently resetting them to the server's defaults.
 * What: `provider` is a plain string, not `ChannelProviderId`: a migrated Gmail
 * listener declares the CONNECTOR id (`gmail`), which the server resolves to
 * the `gworkspace` adapter. `scope` is `#[serde(skip)]` server-side and so
 * appears on neither side of the wire.
 */
export interface GlobalChannel {
  id:string; name:string; provider:string; target:string;
  enabled:boolean; send_enabled:boolean; receive_enabled:boolean;
  transport:string; poll_interval_secs:number; credential_ref?:string;
  instructions:string; event_types:string[];
  /** Assistants this channel fans inbound events out to (owner ruling, option A). */
  route_to:string[];
  ingest_filter:ChannelIngestFilter; wake_filter:ChannelFilter;
}
export interface GlobalChannelConfiguration {scope:string;revision:string;channels:GlobalChannel[];providers:ChannelProvider[];}
export const fetchGlobalChannels=()=>tmApi<GlobalChannelConfiguration>('/api/channels');
/**
 * `PUT /api/channels` — replace the harness-wide list.
 *
 * What: the body is `{revision, channels}` and nothing else. `GlobalUpdate` is
 * `deny_unknown_fields`, so echoing the GET's `providers`/`scope` back is a 422
 * (found live on slice 5, #7609).
 * Test: `channels.global.test.ts::sends only revision and channels…`.
 */
export const saveGlobalChannels=(revision:string,channels:GlobalChannel[])=>withChannelWriteAuth(headers=>tmApi<GlobalChannelConfiguration>('/api/channels',{method:'PUT',headers,body:JSON.stringify({revision,channels})}));

/**
 * The refusal text a missing write credential gets, in BOTH scopes.
 *
 * Why (#7609 slice 6): the daemon's own 401 body talks about `--api-token` in
 * its own words and is free to reword; an operator reading it in the UI needs
 * one sentence that names the two ways out. The stable text is ours, so a
 * server reword never changes what the Channels view says.
 */
export const CHANNEL_WRITE_CREDENTIAL_MESSAGE="Channel writes need the daemon's write credential; restart the daemon or start it with --api-token";

/**
 * True when a failed write lost a compare-and-swap.
 *
 * What: status-first and status-ONLY when there is one. Running the text test
 * alongside a known status made a 422 whose message happens to contain
 * "conflict" — a server wording nothing constrains — reload the list and throw
 * the operator's draft away (critic MEDIUM-1). The regex survives only for a
 * caller, or a test double, that raises a plain `Error` carrying no status.
 * Test: `channels.global.test.ts`.
 */
export function isChannelConflict(cause:unknown):boolean{
  const status=(cause as {status?:number}|null)?.status;
  return typeof status==='number'?status===409:/409|conflict/i.test(String(cause));
}

/**
 * What to show the operator for a failed channel write.
 *
 * What: 401 is the stable sentence above, never the server's. 400 and 422 are
 * the server's own `error` field — it names the field that failed validation,
 * which no client-side wording can improve on. Anything else falls through to
 * the error as rendered, which is what both views did before.
 * Test: `channels.global.test.ts`.
 */
export function channelErrorMessage(cause:unknown):string{
  const status=(cause as {status?:number}|null)?.status;
  if(status===401)return CHANNEL_WRITE_CREDENTIAL_MESSAGE;
  if(status===400||status===422)return cause instanceof Error?cause.message:String(cause);
  return String(cause);
}
