<script lang="ts">
  /**
   * Why (#3932, DOC-57 §6 / §8.2): section 4 of the five-section agent config
   * — "what the agent reacts to". An agent *acts* via skills and *reacts* via
   * listeners; these are inbound API bindings, not MCP tools.
   *
   * This slice moves the section into its normative position (fourth, ahead of
   * Permissions) and makes what it shows honest. Completing the pane is #3937 /
   * #3891 and is deliberately NOT attempted here.
   *
   * What: the connector definitions this UI ships, under a banner saying
   * plainly that they are UI scaffolding rather than this agent's `[[listeners]]`.
   * The per-listener "not bound" badge is GONE: §6.2's L-1 names it as the
   * defect — it was hardcoded, so it asserted a binding state for every agent
   * regardless of configuration, inventing a listener that may not exist and
   * hiding one that is quietly failing. A pane that says "I cannot see your
   * bindings" is honest; one that says "you have none" when it never looked is
   * not. `GET /api/agents/:name/listeners` replaces this constant outright
   * (C-05.1).
   * Test: `AgentConfigListeners.test.ts`.
   */
  import { AlertCircle } from 'lucide-svelte';
  import { DEFINED_LISTENERS } from '../lib/agentConfig';
</script>

<div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
  <p class="flex items-start gap-1.5 rounded-md border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-[11px] text-foundry-amber">
    <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0" />
    <span>
      Scaffolding, not configuration. No route reads an agent's
      <code class="font-mono">[[listeners]]</code> yet (#3937), so nothing below reflects what
      <em>this</em> agent is bound to — these are the connector definitions the UI ships.
    </span>
  </p>
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    Inbound event bindings — API connections to upstream event providers, not MCP tools. Bindings
    live in <code class="font-mono">agent.toml</code>'s
    <code class="font-mono">[[listeners]]</code>.
  </p>
  {#each DEFINED_LISTENERS as listener (listener.id)}
    <div class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2">
      <span class="text-sm font-semibold text-foundry-light-text dark:text-foundry-text">{listener.label}</span>
      <p class="mt-0.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">{listener.description}</p>
      <div class="mt-1.5 flex flex-wrap gap-1">
        {#each listener.eventTypes as eventType (eventType)}
          <span class="rounded border border-dashed border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
            {eventType}
          </span>
        {/each}
      </div>
    </div>
  {/each}
</div>
