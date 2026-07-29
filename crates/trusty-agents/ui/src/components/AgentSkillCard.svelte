<script lang="ts">
  /**
   * Why (#4024): one skill card, rendered in two places — the flat kind buckets
   * of the Skills pane and, for a member of a function skill, inside that
   * bundle's group. Duplicating the markup is how the two drift: a badge or a
   * credential line fixed in one copy and not the other is exactly the quiet
   * inconsistency #3945's honesty rules exist to prevent. Extracting it changes
   * NOTHING about how a card looks — this component is the markup that shipped
   * in PR #3964, moved verbatim.
   *
   * What: one bordered card carrying the skill's human name, its `derived` badge
   * when no manifest named the tool, its prose (or an admission that none was
   * authored), the single tool it wraps (or "guidance only"), and its credential
   * chip via the shared `providerChip` — never a green claim for an OAuth grant
   * this endpoint did not verify.
   * Test: `AgentConfigSkills.test.ts`.
   */
  import type { AgentSkill } from '../lib/agentConfig';
  import { providerChip } from '../lib/agentConfig';

  export let skill: AgentSkill;

  $: credential = skill.provider ? providerChip(skill.provider) : null;
</script>

<div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
  <div class="flex items-baseline justify-between gap-2">
    <span class="text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
      {skill.name}
    </span>
    {#if skill.origin.kind === 'derived'}
      <span
        class="shrink-0 rounded-md bg-foundry-amber/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-amber"
        title="No manifest names this tool — the card was derived from the tool identifier"
      >
        derived
      </span>
    {/if}
  </div>
  {#if skill.description}
    <p class="mt-0.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
      {skill.description}
    </p>
  {:else}
    <p class="mt-0.5 text-[11px] italic text-foundry-light-muted dark:text-foundry-text/40">
      No description authored for this skill.
    </p>
  {/if}
  <div class="mt-1.5 flex flex-wrap items-center gap-1">
    {#each skill.tools as tool (tool)}
      <span
        class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-text dark:text-foundry-text"
        title="The tool this skill wraps"
      >
        {tool}
      </span>
    {/each}
    {#if skill.tools.length === 0}
      <span class="font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
        guidance only — no tool
      </span>
    {/if}
    {#if credential}
      <span
        class="rounded px-1.5 py-0.5 font-mono text-[10px]"
        class:text-foundry-green={credential.tone === 'ok'}
        class:text-foundry-red={credential.tone === 'bad'}
        class:text-foundry-light-muted={credential.tone === 'unknown'}
        title={credential.title}
      >
        {credential.label}
      </span>
    {/if}
  </div>
</div>
