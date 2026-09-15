<script lang="ts">
  /**
   * Why (#7609 slice 6): a global channel fans inbound updates out to the
   * assistants its `route_to` names, so an assistant can be woken by a channel
   * that appears nowhere in its own list. Without this the Assistant scope
   * reads as the complete picture of what reaches it, and it is not.
   *
   * What: read-only. The route lives on the GLOBAL record, so editing it here
   * would need a second writer against `PUT /api/channels` with its own
   * revision — two editors for one list is how a compare-and-swap starts losing
   * writes. The control hands the operator to the Global scope instead.
   * Test: `AssistantRoutedChannels.test.ts`.
   */
  import { onDestroy } from 'svelte';
  import { fetchGlobalChannels, channelErrorMessage, type GlobalChannel } from '../lib/channels';
  export let agent: string;
  /** Switches the Channels view to its Global scope. */
  export let onShowGlobal: () => void;
  let routed: GlobalChannel[] = [], error = '', loaded = '', generation = 0;
  $: if (agent !== loaded) { loaded = agent; void load(agent); }
  async function load(name: string) {
    const token = ++generation;
    routed = []; error = '';
    try {
      const all = await fetchGlobalChannels();
      if (token !== generation) return;
      routed = (all.channels ?? []).filter(channel => channel.route_to.includes(name));
    } catch (cause) {
      if (token === generation) error = channelErrorMessage(cause);
    }
  }
  onDestroy(() => { generation++; });
</script>
{#if error}
  <p class="muted" role="status">Global channels could not be read: {error}</p>
{:else if routed.length}
  <section class="routed" aria-label="Global channels routed here">
    <h3>Also routed here</h3>
    <p class="muted">These channels belong to the whole host and forward their incoming updates to this assistant. Change them in the Global scope.</p>
    <ul>
      {#each routed as channel (channel.id)}
        <li>{channel.name || channel.id} <span class="muted">· {channel.provider}{channel.target ? ` · ${channel.target}` : ' · account-wide'}{channel.enabled ? '' : ' · disabled'}</span></li>
      {/each}
    </ul>
    <button on:click={onShowGlobal}>Edit global channels</button>
  </section>
{/if}
<style>
  .routed { padding:16px; margin:16px 0; border:1px solid rgb(var(--color-border)); border-radius:10px; }
  h3 { font-weight:600; }
  .muted { color:rgb(var(--color-text-muted)); margin:8px 0; }
  ul { margin:8px 0; padding-left:18px; list-style:disc; }
  button { display:inline-flex; align-items:center; gap:6px; border:1px solid rgb(var(--color-border)); border-radius:6px; padding:7px 10px; }
</style>
