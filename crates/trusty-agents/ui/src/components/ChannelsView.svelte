<script lang="ts">
  import { onDestroy } from 'svelte';
  import { Plus, RefreshCw, Trash2, ArrowUp } from 'lucide-svelte';
  import { activeAgentId, agentRoster } from '../stores/app';
  import { CONCIERGE_AGENT_ID, rosterDisplayName } from '../lib/roster';
  import { fetchChannels, saveChannels, fetchChannelMessages, sendChannelMessage, type ChannelConfiguration, type ChannelBinding, type ChannelMessages } from '../lib/channels';
  import AgentConfigListeners from './AgentConfigListeners.svelte';
  let configuration:ChannelConfiguration|null=null;
  let bindings:ChannelBinding[]=[];
  let loading=false, saving=false, sending=false, reading=false, error='', notice='';
  let previous:string|null|undefined=undefined, generation=0, readGeneration=0;
  let selected='', message='', inbox:ChannelMessages|null=null;
  let showListeners=false,listenersVisited=false,listenersDirty=false,listenersSaving=false;
  let viewAgent:string|null=null;
  $: if(showListeners)listenersVisited=true;
  $: protectedEdits=dirty||saving||sending||listenersDirty||listenersSaving||message.trim().length>0;
  $: dirty=configuration!==null && JSON.stringify(bindings)!==JSON.stringify(configuration.bindings);
  $: selectedBinding=configuration?.bindings.find(b=>b.id===selected);
  $: selectedProvider=configuration?.providers.find(p=>p.id===selectedBinding?.provider);
  $: if(previous!==$activeAgentId&&!protectedEdits){previous=$activeAgentId;void load($activeAgentId);}
  async function load(agent:string|null,preserveMessage=false){
    const previousSelection=preserveMessage?selected:'';
    const token=++generation;viewAgent=agent;listenersVisited=false;listenersDirty=false;listenersSaving=false;readGeneration++;configuration=null;bindings=[];selected='';inbox=null;message=preserveMessage?message:'';error='';notice='';loading=!!agent;saving=false;sending=false;reading=false;showListeners=false;
    if(!agent)return;
    try{const result=await fetchChannels(agent);if(token!==generation)return;configuration=result;bindings=structuredClone(result.bindings);if(preserveMessage){selected=result.bindings.some(binding=>binding.id===previousSelection)?previousSelection:'';if(!selected&&message.trim())notice='The previous destination is no longer available. Your unsent message is kept; choose its destination explicitly.';}else selected=result.bindings[0]?.id??'';}
    catch(e){if(token===generation)error=String(e);}
    finally{if(token===generation)loading=false;}
  }
  function add(){
    const provider=configuration?.providers.find(p=>p.configured)??configuration?.providers[0];if(!provider)return;
    bindings=[...bindings,{id:crypto.randomUUID(),name:'New channel',provider:provider.id,target:'',enabled:true,send_enabled:provider.can_send,receive_enabled:false,filter:{from:[],include_labels:[],exclude_labels:[],subject_contains:[],snippet_contains:[]},instructions:''}];
  }
  function changeProvider(index:number){bindings[index].receive_enabled=false;bindings[index].send_enabled=!!configuration?.providers.find(p=>p.id===bindings[index].provider)?.can_send;bindings=[...bindings];}
  async function save(){
    if(!configuration||!viewAgent||saving||!dirty)return;
    const token=generation,agent=viewAgent;saving=true;error='';notice='';
    try{const result=await saveChannels(agent,configuration.revision,bindings);if(token!==generation)return;configuration=result;bindings=structuredClone(result.bindings);readGeneration++;reading=false;inbox=null;if(!result.bindings.some(b=>b.id===selected))selected=message.trim()?'':result.bindings[0]?.id??'';notice=!selected&&message.trim()?'Channels saved. The previous destination is no longer available; choose a destination for your unsent message explicitly.':'Channels saved.';}
    catch(e){if(token===generation)error=/409|conflict/i.test(String(e))?'Settings changed elsewhere. Your edits are still shown. Reload before trying again.':String(e);}
    finally{if(token===generation)saving=false;}
  }
  async function read(){
    if(!viewAgent||!selected||reading||!selectedProvider?.configured||!selectedProvider?.can_read)return;
    const token=generation,request=++readGeneration,agent=viewAgent,id=selected;reading=true;error='';inbox=null;
    try{const result=await fetchChannelMessages(agent,id);if(token===generation&&request===readGeneration&&id===selected)inbox=result;}
    catch(e){if(token===generation&&request===readGeneration)error=String(e);}
    finally{if(token===generation&&request===readGeneration)reading=false;}
  }
  async function send(){
    if(!configuration||!viewAgent||!selectedProvider?.can_send||!selectedBinding?.enabled||!selectedBinding.send_enabled||!selectedProvider?.configured||!message.trim()||sending||dirty)return;
    const token=generation,agent=viewAgent,id=selected,text=message,revision=configuration.revision; sending=true;error='';notice='';
    try{const result=await sendChannelMessage(agent,id,text,revision);if(token!==generation)return;if(!result.ok)throw new Error('Message could not be sent.');message='';notice='Message sent.';await read();}
    catch(e){if(token===generation)error=/409|conflict/i.test(String(e))?'The channel changed elsewhere. Your message has not been sent. Reload the channel settings and check its destination before trying again.':String(e);}
    finally{if(token===generation)sending=false;}
  }
  function selectChannel(){readGeneration++;reading=false;inbox=null;}
  onDestroy(()=>{generation++;readGeneration++;});
  const filterFields=[['from','Senders'],['include_labels','Include labels'],['exclude_labels','Exclude labels'],['subject_contains','Subject contains'],['snippet_contains','Message contains']] as const;
</script>
<section class="channels" aria-label="Assistant channels">
  <header><h2>Channels</h2><select aria-label="Channel assistant" value={viewAgent??''} disabled={protectedEdits} on:change={e=>activeAgentId.set(e.currentTarget.value||null)}><option value="">Select an assistant</option>{#each $agentRoster.filter(a=>a.id!==CONCIERGE_AGENT_ID) as agent (agent.id)}<option value={agent.id}>{rosterDisplayName($agentRoster,agent.id)}</option>{/each}</select></header>
  <div class="body">
    {#if !viewAgent}<p>Select an assistant to configure its channels.</p>{/if}
    {#if viewAgent!==$activeAgentId&&protectedEdits}<p role="status" class="muted">Finish or discard the changes for {rosterDisplayName($agentRoster,viewAgent)} before switching assistants.</p>{/if}
    {#if loading}<p role="status">Loading channels…</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if configuration}
      <p class="muted">Choose where {rosterDisplayName($agentRoster,viewAgent)} can communicate and receive updates.</p>
      {#if bindings.length===0}<p>No channels attached yet.</p>{/if}
      <fieldset disabled={saving||sending}>
      {#each bindings as binding,i (binding.id)}
        {@const provider=configuration.providers.find(p=>p.id===binding.provider)}
        {@const health=configuration.status?.[binding.id]}
        <article>
          <div class="row"><input aria-label="Channel name" bind:value={binding.name} maxlength="100"/><button aria-label={`Remove ${binding.name}`} on:click={()=>bindings=bindings.filter((_,j)=>j!==i)}><Trash2 size={15}/></button></div>
          <div class="row"><label>Service<select bind:value={binding.provider} on:change={()=>changeProvider(i)}>{#each configuration.providers as p}<option value={p.id}>{p.name}</option>{/each}</select></label><label class="grow">Channel or chat ID<input bind:value={binding.target} placeholder={binding.provider==='slack'?'Slack channel ID':'Telegram chat ID'}/></label></div>
          {#if !provider?.configured}<p class="muted">Connect {provider?.name??binding.provider} in the channel service settings to use this channel.</p>{/if}
          <div class="row"><label><input type="checkbox" bind:checked={binding.enabled}/>Enabled</label><label><input type="checkbox" bind:checked={binding.send_enabled} disabled={!provider?.can_send}/>Allow sending</label><label><input type="checkbox" bind:checked={binding.receive_enabled} disabled={!provider?.can_receive}/>Receive updates</label></div>
          {#if binding.receive_enabled&&provider?.receive_reason}<p class="muted">{provider.receive_reason}.</p>{/if}
          {#if !provider?.can_receive}<p class="muted">Automatic updates are not available for this service.</p>{/if}
          <!-- #7427: a binding that has been dropping inbound wakes says so here rather than reading as healthy. -->
          {#if health&&health.dispatch_failures>0}<p class="error" role="status">{health.dispatch_failures} incoming {health.dispatch_failures===1?'message':'messages'} could not reach the assistant since this service started.{#if health.last_error}<br/>Last error: {health.last_error}{/if}</p>{/if}
          {#if binding.receive_enabled}<details><summary>Update filters and instructions</summary><p class="muted">One value per line. Match any value within a field and every configured field. Excluded labels take priority.</p>{#each filterFields as [key,label]}<label>{label}<textarea rows="2" value={(binding.filter?.[key]??[]).join('\n')} on:input={e=>{binding.filter={...binding.filter,[key]:e.currentTarget.value.split('\n').map(v=>v.trim()).filter(Boolean)};bindings=[...bindings];}}></textarea></label>{/each}<label>Instructions for this channel<textarea rows="4" maxlength="8000" bind:value={binding.instructions}></textarea></label></details>{/if}
        </article>
      {/each}
      </fieldset>
      <div class="row"><button on:click={add} disabled={saving||sending||configuration.providers.length===0}><Plus size={14}/>Add channel</button><button class="primary" on:click={save} disabled={!dirty||saving||sending}>{saving?'Saving…':'Save channels'}</button><button on:click={()=>load(viewAgent,true)} disabled={saving||sending||listenersDirty||listenersSaving}><RefreshCw size={14}/>{dirty?'Discard changes and reload':'Reload'}</button></div>
      {#if configuration.bindings.length>0||message.trim()}<section class="conversation"><h3>Channel messages</h3><div class="row"><select aria-label="Selected channel" bind:value={selected} on:change={selectChannel} disabled={sending}><option value="">Select a destination…</option>{#each configuration.bindings as binding}<option value={binding.id}>{binding.name}</option>{/each}</select><button on:click={read} disabled={reading||!selectedProvider?.configured||!selectedProvider?.can_read}>{reading?'Loading…':'Refresh messages'}</button></div>
      {#if selectedProvider&&!selectedProvider.can_read}<p class="muted">This service does not provide message history.</p>{/if}
      {#if inbox}{#if !inbox.available}<p class="muted">{inbox.reason??'Messages unavailable.'}</p>{/if}{#each inbox.messages as row (row.id)}<div class="message">{#if row.from}<strong>{row.from}</strong>{/if}<p>{row.text}</p></div>{/each}{/if}
      <div class="composer"><textarea aria-label="Channel message" bind:value={message} placeholder="Message this channel…" disabled={sending||dirty}></textarea><button aria-label="Send channel message" class="primary" on:click={send} disabled={sending||dirty||!message.trim()||!selectedBinding?.send_enabled||!selectedBinding?.enabled||!selectedProvider?.configured||!selectedProvider?.can_send}><ArrowUp size={16}/></button></div></section>{/if}
      <button on:click={()=>showListeners=!showListeners}>{showListeners?'Hide':'Configure'} other update sources</button>
      {#if listenersVisited&&viewAgent}<div style:display={showListeners?'block':'none'} data-channel-listeners><AgentConfigListeners agentName={viewAgent} bind:dirty={listenersDirty} bind:saving={listenersSaving}/></div>{/if}
    {/if}
  </div>
</section>
<style>
 .channels{display:flex;flex:1;min-width:0;min-height:0;flex-direction:column;color:rgb(var(--color-text-primary));font-size:13px}header{display:flex;align-items:center;gap:18px;padding:14px 20px;border-bottom:1px solid rgb(var(--color-border))}h2,h3{font-weight:600}h2{font-size:16px}.body{overflow:auto;padding:20px;min-height:0}article,.conversation{padding:16px;margin:16px 0;border:1px solid rgb(var(--color-border));border-radius:10px}fieldset{border:0;padding:0;min-width:0}.row{display:flex;gap:12px;align-items:center;flex-wrap:wrap;margin:10px 0}.grow{flex:1}.muted{color:rgb(var(--color-text-muted));margin:8px 0}.error{color:#ba4520}label{display:block}label input[type=checkbox]{margin-right:6px}input:not([type=checkbox]),select,textarea{border:1px solid rgb(var(--color-border));border-radius:6px;padding:8px;background:rgb(var(--color-card-bg));color:inherit;max-width:100%}label input:not([type=checkbox]),label select,label textarea{display:block;width:100%;margin-top:5px}textarea{width:100%;resize:vertical}button{display:inline-flex;align-items:center;gap:6px;border:1px solid rgb(var(--color-border));border-radius:6px;padding:7px 10px}button:disabled{opacity:.45;cursor:default}.primary{background:rgb(var(--color-primary));color:white}.composer{display:flex;align-items:flex-end;gap:10px;margin-top:16px}.composer textarea{flex:1;min-width:0}.message{padding:10px 0;border-bottom:1px solid rgb(var(--color-border))}.message p{white-space:pre-wrap;overflow-wrap:anywhere}details label{margin-top:10px}summary{cursor:pointer}
</style>
