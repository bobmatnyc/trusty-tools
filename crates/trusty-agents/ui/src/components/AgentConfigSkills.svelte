<script lang="ts">
  /**
   * Why (#3933, DOC-57 §5 / §8.2): section 3 of the five-section agent config
   * — "what the agent can do". Owner decision, 2026-07-25, resolving OQ-2:
   * *"One skill per tool. There can be other skills without tools, but each
   * [tool] needs an accompanying skill"*, with human, provider-recognisable
   * names ("MTA Train Time", not `get_train_schedule`).
   *
   * This replaces #3932's placeholder, which grouped `[tools].allow` globs by
   * their `_` prefix and badged every card `synthetic`. That was honest about
   * being a guess, but it showed the user prefix fragments rather than
   * capabilities and could not say whether any of them resolved. The cards here
   * come from `GET /api/agents/:name/skills`, which resolves grants through the
   * SAME matcher the dispatch gate uses.
   *
   * What: one card per granted skill, carrying its human name, the single tool
   * it wraps, and its credential requirement. Ungranted skills are counted, not
   * listed — the pane answers "what can this agent do", and editing arrives as
   * `PATCH { skills_allow }` (§5.7, S-12). Read-only here, deliberately.
   *
   * Honesty rules kept from #3945: a credential renders CONFIGURED only when an
   * environment variable actually resolved; an OAuth grant renders "not
   * verified" rather than a green chip nobody checked; a `derived` skill (a
   * live-discovered MCP tool with no authored manifest) is badged as such; and
   * an allow-pattern that resolved to nothing is shown with its reason instead
   * of being dropped.
   * Test: `AgentConfigSkills.test.ts`.
   */
  import type { AgentSkills, AgentSkill } from '../lib/agentConfig';

  /** Loaded by the shell from `GET /api/agents/:name/skills`; `null` while loading. */
  export let data: AgentSkills | null = null;
  /** Non-empty when the fetch itself failed. */
  export let error = '';

  // Every array is defaulted rather than assumed: an older sidecar (or a route
  // that degraded) can return a payload missing a field, and a pane that throws
  // on that shows the user nothing at all — strictly worse than showing what
  // did arrive.
  $: all = data?.skills ?? [];
  $: unresolved = data?.unresolved ?? [];
  $: unmatched = data?.unmatched_patterns ?? [];
  $: granted = all.filter((s) => s.granted);
  $: actions = granted.filter((s) => s.kind === 'action');
  $: knowledge = granted.filter((s) => s.kind === 'knowledge');
  $: system = granted.filter((s) => s.kind === 'system');
  $: catalogSize = all.length;

  /** Credential line for a card, or `null` when the skill needs none. */
  function credential(skill: AgentSkill): { label: string; tone: string; title: string } | null {
    const p = skill.provider;
    if (!p) return null;
    if (p.configured === true)
      return { label: `${p.provider} configured`, tone: 'ok', title: p.requirement };
    if (p.configured === false)
      return {
        label: `${p.provider} NOT configured`,
        tone: 'bad',
        title: `${p.requirement} Set ${p.env_var}.`,
      };
    return { label: `${p.provider} — not verified`, tone: 'unknown', title: p.requirement };
  }
</script>

<div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    What this agent can do. Each card is one skill wrapping exactly one tool, named for what
    invoking it accomplishes. Edit the grants in <code class="font-mono">agent.toml</code> —
    <code class="font-mono">[skills].allow</code> (skill ids) or
    <code class="font-mono">[tools].allow</code> (tool globs); the two are unioned, so neither
    removes what the other grants. Editing from here arrives in a later phase (DOC-57 §5.7).
  </p>

  {#if error}
    <p class="rounded-md border border-foundry-red/40 px-3 py-2 text-[11px] text-foundry-red">
      Could not load skills: {error}
    </p>
  {:else if !data}
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">Loading skills…</p>
  {:else}
    {#if data.config_error}
      <p class="rounded-md border border-foundry-amber/40 px-3 py-2 text-[11px] text-foundry-amber">
        <code class="font-mono">agent.toml</code> could not be parsed, so no grant could be resolved:
        {data.config_error}
      </p>
    {/if}

    {#if !data.declares_capability}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        This agent declares neither <code class="font-mono">[skills].allow</code> nor
        <code class="font-mono">[tools].allow</code>. On the persona chat path that grants
        <strong>no</strong> skills — it is not "unrestricted" (DOC-57 §7.1).
      </p>
    {:else if granted.length === 0}
      <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
        This agent declares capability but no declared grant resolves to a known skill. See the
        unresolved entries below.
      </p>
    {/if}

    {#each [{ label: 'Actions', items: actions }, { label: 'Knowledge', items: knowledge }, { label: 'System', items: system }] as group (group.label)}
      {#if group.items.length > 0}
        <h4 class="mt-1 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          {group.label} ({group.items.length})
        </h4>
        {#each group.items as skill (skill.id)}
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
              {#if credential(skill)}
                {@const c = credential(skill)}
                <span
                  class="rounded px-1.5 py-0.5 font-mono text-[10px]"
                  class:text-foundry-green={c?.tone === 'ok'}
                  class:text-foundry-red={c?.tone === 'bad'}
                  class:text-foundry-light-muted={c?.tone === 'unknown'}
                  title={c?.title}
                >
                  {c?.label}
                </span>
              {/if}
            </div>
          </div>
        {/each}
      {/if}
    {/each}

    {#if unresolved.length > 0}
      <div class="rounded-md border border-foundry-amber/40 px-3 py-2">
        <h4 class="font-mono text-[10px] uppercase tracking-wide text-foundry-amber">
          Unresolved skill grants ({unresolved.length})
        </h4>
        {#each unresolved as u (u.id)}
          <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
            <code class="font-mono">{u.id}</code> — {u.reason}
          </p>
        {/each}
      </div>
    {/if}

    {#if unmatched.length > 0}
      <details class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2">
        <summary class="cursor-pointer font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          {unmatched.length} allow-pattern{unmatched.length === 1
            ? ''
            : 's'} with no catalog skill
        </summary>
        {#each unmatched as p (p.pattern)}
          <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
            <code class="font-mono">{p.pattern}</code> — {p.reason}
          </p>
        {/each}
      </details>
    {/if}

    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      {granted.length} of {catalogSize} known skills granted.
    </p>
  {/if}
</div>
