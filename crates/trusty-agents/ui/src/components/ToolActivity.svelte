<script lang="ts">
  import { Search, Terminal, FileText, Globe, Wrench, ChevronRight, Loader2, AlertCircle } from 'lucide-svelte';
  export let name: string;
  export let details = '';
  export let status: string | undefined = undefined;
  $: readableName = name.trim().replace(/\\_/g, '_').replace(/_+/g, ' ');
  $: label = readableName ? readableName[0].toUpperCase() + readableName.slice(1) : 'Tool activity';
  $: icon = /search|find|query|lookup/i.test(label) ? Search
    : /shell|exec|command|terminal|bash/i.test(label) ? Terminal
    : /file|read|write|edit|patch/i.test(label) ? FileText
    : /web|browse|fetch|url/i.test(label) ? Globe : Wrench;
</script>

<div class="tool-activity" data-tool-activity>
  {#if details.trim()}
    <details>
      <summary aria-label={`Tool activity: ${label}`}>
        <ChevronRight size={12} class="chevron" aria-hidden="true" />
        <svelte:component this={icon} size={14} aria-hidden="true" />
        <span class="name" title={name}>{label}</span>
        {#if status === 'running'}<Loader2 size={12} class="animate-spin" aria-label="Running" />{:else if status === 'error'}<AlertCircle size={12} aria-label="Failed" />{/if}
      </summary>
      <pre>{details}</pre>
    </details>
  {:else}
    <div class="activity-label" aria-label={`Tool activity: ${label}`}>
      <svelte:component this={icon} size={14} aria-hidden="true" /><span class="name" title={name}>{label}</span>
      {#if status === 'running'}<Loader2 size={12} class="animate-spin" aria-label="Running" />{:else if status === 'error'}<AlertCircle size={12} aria-label="Failed" />{/if}
    </div>
  {/if}
</div>
<style>
  .tool-activity { width:100%; min-width:0; color:rgb(var(--color-text-muted)); font-size:12px; }
  summary,.activity-label { display:flex; align-items:center; gap:7px; min-height:25px; }
  summary { cursor:pointer; list-style:none; }
  summary::-webkit-details-marker { display:none; }
  summary:focus-visible { outline:2px solid rgb(var(--color-primary)); outline-offset:3px; border-radius:4px; }
  .name { overflow-wrap:anywhere; }
  .activity-label { padding-left:19px; }
  summary :global(svg),.activity-label :global(svg) { flex-shrink:0; }
  details[open] summary :global(.chevron) { transform:rotate(90deg); }
  pre { margin:6px 0 0 19px; padding:10px 12px; white-space:pre-wrap; overflow-wrap:anywhere; max-height:260px; overflow:auto; border-radius:8px; background:rgb(var(--color-text-primary) / .04); font-size:11px; line-height:1.6; }
</style>
