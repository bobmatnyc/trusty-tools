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
   *
   * #4024 — function skills render as GROUPS. The owner's directive is that
   * skills be organized by function, so a `kind: 'function'` bundle is a group
   * header over its member cards rather than a card of its own. Three rules keep
   * it honest:
   *   - The tri-state is rendered as a tri-state. A header says "granted" only
   *     when `granted_state === 'all'`; `some` reads "partial" with the counts,
   *     `none` reads "not granted". A bundle is never claimed as granted because
   *     it was named.
   *   - Nothing about membership or grant state is derived here. Both come from
   *     `groups[]`, which the route builds from the SAME computation as the
   *     cards — PR #3964 deleted `synthesizeSkills` precisely so the pane cannot
   *     drift from the enforcement path, and re-deriving would reintroduce it.
   *   - The counts do not move. A bundle is not a capability (S-16), so the
   *     "N of M known skills granted" footer still counts leaves only, matching
   *     the server's `granted_count`. A member is shown INSIDE its group instead
   *     of in the flat kind buckets, so it renders once, not twice.
   * Test: `AgentConfigSkills.test.ts`.
   */
  import type { AgentSkills, AgentSkill, AgentSkillGroup } from '../lib/agentConfig';
  import { providerChip } from '../lib/agentConfig';
  import AgentSkillCard from './AgentSkillCard.svelte';

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
  // #4022: a `function` card is a BUNDLE — a group header over its member
  // cards, not a capability of its own. It is excluded from both counts for the
  // same reason the server excludes it from `granted_count`: granting
  // `ticketing` adds twelve capabilities, and counting the header as a
  // thirteenth would print "13 of N" over twelve rendered cards. #4024 renders
  // the header itself, from `groups[]` — never from these cards.
  $: leaves = all.filter((s) => s.kind !== 'function');
  $: granted = leaves.filter((s) => s.granted);
  // #3987: scope grants that can match nothing. Defaulted like every array
  // above so an older sidecar (which has no such field) renders the rest of
  // the pane rather than throwing.
  $: deadScopes = data?.dead_scope_patterns ?? [];
  // #4024: the function tier. `groups` is the route's index over the
  // `kind: 'function'` cards; `byId` resolves a member id back to its card so a
  // group renders REAL cards rather than a second, thinner rendering of them.
  $: groups = data?.groups ?? [];
  $: byId = new Map(all.map((s) => [s.id, s]));
  // A member shown inside its group is removed from the flat buckets: one card,
  // one place. Membership is read from the group (what the bundle declares), not
  // inferred from the card, so an ungranted member is pulled out too — it is not
  // listed anywhere, exactly as an ungranted leaf is not.
  $: groupedIds = new Set(groups.flatMap((g) => g.members));
  $: ungrouped = granted.filter((s) => !groupedIds.has(s.id));
  $: actions = ungrouped.filter((s) => s.kind === 'action');
  $: knowledge = ungrouped.filter((s) => s.kind === 'knowledge');
  $: system = ungrouped.filter((s) => s.kind === 'system');
  $: catalogSize = leaves.length;

  /**
   * The group header's grant word, its tone, and the counts behind it.
   *
   * Why: The one place the tri-state could be flattened back into a boolean, so
   * it is the one place worth stating: "granted" is reserved for `all`. `some`
   * says "partial" and shows both numbers, because "7 of 12" is the fact the
   * user needs and "granted" would be a claim about five skills this agent does
   * not hold.
   */
  function groupState(g: AgentSkillGroup): { word: string; tone: string; counts: string } {
    const counts = `${g.granted_members.length} of ${g.members.length} skills`;
    if (g.granted_state === 'all') return { word: 'granted', tone: 'ok', counts };
    if (g.granted_state === 'some') return { word: 'partial', tone: 'warn', counts };
    return { word: 'not granted', tone: 'off', counts };
  }

  /** The member cards a group renders — granted only, in declared order. */
  function grantedCards(g: AgentSkillGroup): AgentSkill[] {
    return g.granted_members
      .map((id) => byId.get(id))
      .filter((s): s is AgentSkill => s !== undefined);
  }
</script>

<div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    What this agent can do. Each card below is one skill wrapping exactly one tool, named for what
    invoking it accomplishes. Skills that belong to a <em>function skill</em> — a bundle such as
    <code class="font-mono">ticketing</code> that grants all of its members at once — are shown
    under that function; a function reads <em>granted</em> only when every one of its members is.
    Edit the grants in
    <code class="font-mono">agent.toml</code> —
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

    <!--
      #4024: the function tier, rendered first because it is the organising
      layer the owner asked for — "skills organized by function". Each group is
      collapsed by default (a header is a summary, not a wall of cards) and its
      grant word comes straight from the route's tri-state.
    -->
    {#if groups.length > 0}
      <h4 class="mt-1 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
        Functions ({groups.length})
      </h4>
      {#each groups as g (g.id)}
        {@const state = groupState(g)}
        {@const cards = grantedCards(g)}
        <details class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
          <summary class="cursor-pointer list-none">
            <span class="flex flex-wrap items-baseline justify-between gap-2">
              <span class="text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
                {g.name}
              </span>
              <span
                class="shrink-0 rounded-md px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide {state.tone ===
                'ok'
                  ? 'bg-foundry-green/15 text-foundry-green'
                  : state.tone === 'warn'
                    ? 'bg-foundry-amber/15 text-foundry-amber'
                    : 'text-foundry-light-muted dark:text-foundry-text/40'}"
                title="A bundle is reported granted only when EVERY member skill is granted"
              >
                {state.word} — {state.counts}
              </span>
            </span>
          </summary>
          <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
            {g.description}
          </p>
          <!--
            The bundle's credential requirements, as a SET. Two members needing
            two different credentials render as two chips — a divergence is
            shown, never averaged into one verdict (#4024, DOC-57 S-16).
          -->
          {#if (g.providers ?? []).length > 0}
            <div class="mt-1 flex flex-wrap items-center gap-1">
              {#each g.providers ?? [] as p (p.provider + p.requirement)}
                {@const c = providerChip(p)}
                <span
                  class="rounded px-1.5 py-0.5 font-mono text-[10px]"
                  class:text-foundry-green={c.tone === 'ok'}
                  class:text-foundry-red={c.tone === 'bad'}
                  class:text-foundry-light-muted={c.tone === 'unknown'}
                  title={c.title}
                >
                  {c.label}
                </span>
              {/each}
            </div>
          {/if}
          <div class="mt-1.5 flex flex-col gap-2">
            {#each cards as skill (skill.id)}
              <AgentSkillCard {skill} />
            {/each}
          </div>
          <!--
            Ungranted members are counted, not listed — the same policy the flat
            buckets follow. Saying how many are missing is what keeps "partial"
            from being a shrug.
          -->
          {#if g.members.length > g.granted_members.length}
            <p class="mt-1.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
              {g.members.length - g.granted_members.length} member skill{g.members.length -
                g.granted_members.length ===
              1
                ? ''
                : 's'} in this function {g.members.length - g.granted_members.length === 1
                ? 'is'
                : 'are'} not granted. Grant the whole function with
              <code class="font-mono">{g.id}</code> in
              <code class="font-mono">[skills].allow</code>.
            </p>
          {/if}
        </details>
      {/each}
    {/if}

    {#each [{ label: 'Actions', items: actions }, { label: 'Knowledge', items: knowledge }, { label: 'System', items: system }] as bucket (bucket.label)}
      {#if bucket.items.length > 0}
        <h4 class="mt-1 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          {bucket.label} ({bucket.items.length})
        </h4>
        {#each bucket.items as skill (skill.id)}
          <AgentSkillCard {skill} />
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

    <!--
      #3987: dead scope grants. Given the SAME visual weight as
      "Unresolved skill grants" above rather than the muted `details`
      treatment used for `unmatched_patterns` below, because the two are not
      equally severe: an unmatched allow-pattern MAY still resolve to a
      live-discovered MCP tool, whereas a dead scope pattern conclusively
      denies every scoped tool it names. Without this the pane would render
      those tools as granted cards while dispatch silently drops them — the
      exact confusion this issue exists to end.
    -->
    {#if deadScopes.length > 0}
      <div class="rounded-md border border-foundry-amber/40 px-3 py-2">
        <h4 class="font-mono text-[10px] uppercase tracking-wide text-foundry-amber">
          Dead scope grants ({deadScopes.length})
        </h4>
        {#each deadScopes as d (d.pattern)}
          <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
            <code class="font-mono">{d.pattern}</code> — {d.reason}
            {#if d.nearest.length > 0}
              <span class="block mt-0.5">
                Nearest reachable: {#each d.nearest as n, i (n)}<code class="font-mono"
                    >{n}</code
                  >{i < d.nearest.length - 1 ? ', ' : ''}{/each}
              </span>
            {/if}
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
