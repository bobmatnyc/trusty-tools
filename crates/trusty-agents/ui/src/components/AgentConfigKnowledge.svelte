<script lang="ts">
  /**
   * Why (#3932, DOC-57 §4 / §8.2): section 2 of the five-section agent config
   * — "what the agent knows". This replaces the OKG Stores tab, and §8.2's G-2
   * is explicit that the change is not a label swap: Knowledge is ONE pane over
   * THREE sub-surfaces, because a user asking "what does this agent know?" must
   * not have to correlate a store card, a tool glob and a harness endpoint
   * table across three files.
   *
   * What: K-a store bindings (live, #3878 — the only sub-surface with a route
   * in this phase); K-b the store-bound knowledge tool, resolved against the
   * agent's own allow-list so the `vector_search`→store linkage §4.3 requires
   * is visible; K-c the declared MCP knowledge endpoints. Read-only throughout
   * — store editing is #3890's contract (§4.5, K-3).
   *
   * The pane's posture is #3878's, inherited by §4.2: declarative data only,
   * degrade never fail, and never fabricate (G-4). K-a distinguishes loading /
   * empty / error as three states. K-b states the classification gap rather
   * than guessing at one — `kind = "knowledge"` manifests are Phase 2 (#3933).
   * K-c reports the shipped defaults as declared-not-probed, and shows a
   * disabled endpoint as DISABLED with its reason instead of hiding it
   * (C-03.2).
   * Test: `AgentConfigKnowledge.test.ts`.
   */
  import { AlertCircle, Loader2 } from 'lucide-svelte';
  import { KNOWLEDGE_MCP_ENDPOINTS, type OkgStoreBinding } from '../lib/agentConfig';

  /** `null` until the first load resolves — "loading", not "binds nothing". */
  export let stores: OkgStoreBinding[] | null = null;
  export let issues: string[] = [];
  export let error = '';
  /**
   * Store-bound knowledge tools this agent's `[tools].allow` actually grants
   * (DOC-57 §4.3). Resolved by the shell; empty is a real answer, not a gap.
   */
  export let storeBoundTools: string[] = [];

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
</script>

<div class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
  <section class="flex flex-col gap-2">
    <h3 class={heading}>Store bindings</h3>
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      Knowledge trees + search indexes bound to this agent, resolved live against trusty-search and
      trusty-memory. Edit bindings in <code class="font-mono">agent.toml</code>'s
      <code class="font-mono">[[stores]]</code>.
    </p>
    {#if stores === null}
      <div class="flex items-center gap-2 text-xs text-foundry-light-muted dark:text-foundry-text/60">
        <Loader2 class="h-3.5 w-3.5 animate-spin" /> Resolving stores…
      </div>
    {:else if error}
      <p class="flex items-center gap-1.5 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-[11px] text-red-500 dark:text-red-400">
        <AlertCircle class="h-3.5 w-3.5 shrink-0" /> Could not read store bindings: {error}
      </p>
    {:else if stores.length === 0}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        This agent binds no OKG store. Add a <code class="font-mono">[[stores]]</code> table to its
        <code class="font-mono">agent.toml</code> to give it a knowledge tree.
      </p>
    {/if}
    {#each stores ?? [] as store (store.name)}
      <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
        <div class="flex items-center justify-between gap-2">
          <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{store.name}</span>
          <span
            class="shrink-0 rounded-md px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide {store.connected
              ? 'bg-foundry-teal/15 text-foundry-teal'
              : 'bg-foundry-light-border/50 dark:bg-black/30 text-foundry-light-muted dark:text-foundry-text/50'}"
          >
            {store.connected ? 'connected' : 'not connected'}
          </span>
        </div>
        <div class="mt-1 flex flex-col gap-0.5 font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
          <span>tree: {store.tree}</span>
          <span>
            index: {store.index}{#if store.connected && store.chunk_count !== undefined}
              &nbsp;· {store.chunk_count.toLocaleString()} chunks{/if}{#if store.connected && store.index_status}
              &nbsp;· {store.index_status}{/if}
          </span>
          {#if store.root_path}<span>root: {store.root_path}</span>{/if}
          {#if store.palace}
            <span>
              palace: {store.palace}
              {#if store.palace_connected === false}
                <span class="text-foundry-amber">— {store.palace_reason ?? 'not connected'}</span>
              {/if}
            </span>
          {/if}
        </div>
        {#if !store.connected && store.reason}
          <p class="mt-1.5 text-[11px] text-foundry-amber">{store.reason}</p>
        {/if}
        <!-- §4.3: `vector_search`'s default index IS this binding (#3864), so
             the tool renders UNDER the store it reads, not as a free-floating
             chip elsewhere in the pane. -->
        {#if storeBoundTools.length > 0}
          <div class="mt-2 flex flex-wrap items-center gap-1.5 border-t border-foundry-light-border/60 dark:border-foundry-border/60 pt-2">
            <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
              reads via
            </span>
            {#each storeBoundTools as tool (tool)}
              <span class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-text dark:text-foundry-text">
                {tool}
              </span>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
    {#each issues as issue (issue)}
      <p class="text-[11px] text-foundry-amber">{issue}</p>
    {/each}
  </section>

  <section class="flex flex-col gap-2">
    <h3 class={heading}>Knowledge tools</h3>
    {#if storeBoundTools.length === 0}
      <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
        This agent's allow-list grants no store-bound knowledge tool.
      </p>
    {:else}
      <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
        Bound to the store above: <span class="font-mono">{storeBoundTools.join(', ')}</span>.
      </p>
    {/if}
    <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      A capability is knowledge-classified when the skill wrapping it declares
      <code class="font-mono">kind = "knowledge"</code> (DOC-57 §4.3) — the only classification
      mechanism there is, deliberately not a hardcoded tool list. No manifest carries that key yet
      (#3933), so nothing is routed out of <strong>Skills</strong> — which still lists every
      capability, including the one above — and nothing is guessed into this section.
    </p>
  </section>

  <section class="flex flex-col gap-2">
    <h3 class={heading}>MCP knowledge connections</h3>
    <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
      Knowledge services reachable over MCP/OpenRPC, as <em>declared</em> in the shipped
      <code class="font-mono">config.toml</code> defaults — not probed. Live status arrives with
      <code class="font-mono">GET /api/agents/:name/knowledge</code> (DOC-57 §4.5).
    </p>
    {#each KNOWLEDGE_MCP_ENDPOINTS as endpoint (endpoint.name)}
      <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
        <div class="flex items-center justify-between gap-2">
          <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{endpoint.name}</span>
          <span
            class="shrink-0 rounded-md px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide {endpoint.enabled
              ? 'bg-foundry-teal/15 text-foundry-teal'
              : 'bg-foundry-light-border/50 dark:bg-black/30 text-foundry-light-muted dark:text-foundry-text/50'}"
          >
            {endpoint.enabled ? 'enabled' : 'disabled'}
          </span>
        </div>
        <p class="mt-0.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
          {endpoint.description}
        </p>
        <div class="mt-1.5 flex flex-wrap gap-1">
          {#each endpoint.scopes as scope (scope)}
            <span class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/50">
              {scope}
            </span>
          {/each}
        </div>
        {#if endpoint.reason}
          <p class="mt-1.5 text-[11px] text-foundry-amber">{endpoint.reason}</p>
        {/if}
      </div>
    {/each}
  </section>
</div>
