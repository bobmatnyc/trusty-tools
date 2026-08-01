<script lang="ts">
  /**
   * Why (#4353, spec DOC-62): the THIRD delegation mechanism — coding work
   * handed to the external trusty-code project manager. #4350 made that target
   * addressable by the reserved name `coding-pm`, but nothing surfaced it, so
   * an operator could not see that the lane exists, could not see that
   * `[subagents].allowed` does NOT gate it (every other cross-product target is
   * allow-list gated, so the natural assumption is wrong), and could not see
   * what a style request would actually do.
   *
   * That last one is why this section is not just a card. DOC-62 §5.4 makes a
   * caller-supplied style a CEILING REQUEST the callee may raise but never
   * lower, and SM-9 raises `vibe` to `engineer` for as long as #2596 is open;
   * the coding lane's own floor raises `hack` to `vibe` first. So TODAY every
   * request to this lane resolves to `engineer` — a selector that echoed its own
   * selected value would be stating something false on every setting. DOC-62
   * OQ-6 answers exactly this: expose the per-delegation override, but render
   * the EFFECTIVE style and its resolution path, "so the override is honest
   * rather than decorative". This pane does that, and it does it without
   * computing anything: every resolution below is produced server-side by the
   * real `ResolvedStyle::resolve` (see `agent_subagents.rs::coding_surface`),
   * so it cannot disagree with the bridge and cannot go stale the day #2596
   * lands.
   *
   * OQ-3 (`engineer` the style vs `engineer` the tcode sub-agent role) is
   * mitigated presentationally, per DOC-62's recommendation: labels come from
   * `lib/executionStyle.ts`, never from the raw wire value. That recommendation
   * is NOT formally ratified.
   *
   * Read-only, matching every sibling section. `[subagents] default_style` has
   * no write route (`PATCH /api/agents/:name` accepts model/provider/personality
   * only), and DOC-62 OQ-1 — whether the config default is per-assistant, global,
   * or both — is still open, so persisting a default here would build on an
   * unratified decision. The selector is therefore a per-delegation control over
   * what WOULD happen, which is the question SM-9 makes urgent; the config
   * default is reported, and edited in `agent.toml`.
   * Test: `AgentConfigCoding.test.ts`.
   */
  import type { ExecutionStyle, SubagentCoding } from '../lib/agentConfig';
  import {
    STYLE_PRESENTATION,
    escalationSentence,
    resolutionFor,
    sourceSentence,
    wasOverruled,
  } from '../lib/executionStyle';

  /** The `coding` half of `GET /api/agents/:name/subagents`; `undefined` on an
   * older sidecar that does not report the lane. */
  export let data: SubagentCoding | undefined = undefined;

  /**
   * The per-delegation override under consideration. `null` is the no-override
   * row — "let the assistant decide", i.e. config default or built-in.
   *
   * Local, not persisted: this control asks "what would happen if I asked for
   * this", which is answerable without a write route (see the header comment).
   */
  let requested: ExecutionStyle | null = null;

  // Every field is defaulted rather than assumed — same posture as the sibling
  // panes, so a payload missing a key renders what did arrive instead of
  // throwing and showing nothing.
  $: styles = (data?.resolutions ?? [])
    .map((r) => r.caller)
    .filter((c): c is ExecutionStyle => c !== null);
  $: resolution = resolutionFor(data, requested);
  $: overruled = resolution ? wasOverruled(resolution) : false;

  const heading =
    'shrink-0 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const okChip =
    'shrink-0 rounded-md bg-foundry-teal/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-teal';
  const offChip =
    'shrink-0 rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50';
  const tag =
    'rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-text dark:text-foundry-text';
</script>

<section class="flex flex-col gap-2">
  <div class="flex items-center justify-between gap-2">
    <h3 class={heading}>
      Coding · <code class="font-mono">{data?.target ?? 'coding-pm'}</code>
    </h3>
    <span class={data?.tool_granted ? okChip : offChip}>
      {data?.tool_granted ? 'granted' : 'not granted'}
    </span>
  </div>

  {#if !data}
    <p
      class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40"
    >
      This server does not report the coding delegation lane, so nothing about it is shown here
      rather than a stand-in that could disagree with what the bridge does. Upgrade the
      <code class="font-mono">tagent</code> sidecar to see it.
    </p>
  {:else}
    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
      The external trusty-code project manager — the <strong>only</strong> coding delegation
      surface, and the one target that plans and writes code. It runs out-of-process on the same
      <code class="font-mono">{data.tool}</code> tool as the non-coding specialists above, but it is
      a separate lane: the reserved name is recognised <em>before</em> the non-coding allow-set is
      consulted, so
      <code class="font-mono">[subagents].allowed</code> does not gate it — listing it there does
      nothing. Holding <code class="font-mono">{data.tool}</code> is the whole reachability
      condition. No coding sub-agent is reachable from here; the project manager is the boundary,
      and it is enforced in code.
    </p>

    {#if !data.tool_granted}
      <p
        class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-amber"
      >
        This agent does not hold <code class="font-mono">{data.tool}</code>, so it cannot address
        the coding project manager at all. The styles below describe what would happen if it could.
      </p>
    {/if}

    <!-- ── Style selector (DOC-62 §5, OQ-6) ─────────────────────────────── -->
    <fieldset class="flex flex-col gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
      <legend class="px-1 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
        Execution style
      </legend>
      <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
        How much ceremony a coding delegation runs. A style is a <strong>ceiling request</strong>:
        the project manager may apply more ceremony than you ask for and never less, so pick one and
        read what it actually resolves to below. Style selects only the delegate's own process — it
        never relaxes a check this repository enforces (CI, branch protection, required review) and
        it grants no capability.
      </p>

      <label class="flex items-start gap-2 text-[11px] text-foundry-light-text dark:text-foundry-text">
        <input
          type="radio"
          class="mt-0.5"
          name="execution-style"
          value=""
          checked={requested === null}
          on:change={() => (requested = null)}
        />
        <span>
          <span class="font-semibold">No override</span>
          <span class="text-foundry-light-muted dark:text-foundry-text/50">
            — let the resolved default apply.
          </span>
        </span>
      </label>

      {#each styles as style (style)}
        <label class="flex items-start gap-2 text-[11px] text-foundry-light-text dark:text-foundry-text">
          <input
            type="radio"
            class="mt-0.5"
            name="execution-style"
            value={style}
            checked={requested === style}
            on:change={() => (requested = style)}
          />
          <span>
            <span class="font-semibold">{STYLE_PRESENTATION[style].label}</span>
            <span class="text-foundry-light-muted dark:text-foundry-text/50">
              — {STYLE_PRESENTATION[style].meaning}
            </span>
          </span>
        </label>
      {/each}
    </fieldset>

    <!-- ── Effective style + resolution path (DOC-62 §3.4) ──────────────── -->
    {#if resolution}
      <div
        class="rounded-md border px-3 py-2 {overruled
          ? 'border-foundry-amber/40'
          : 'border-foundry-light-border dark:border-foundry-border'}"
      >
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
            Effective style: {STYLE_PRESENTATION[resolution.effective].label}
          </span>
          <span class={overruled ? offChip : okChip}>
            {overruled ? 'raised' : 'as requested'}
          </span>
        </div>
        <div class="mt-1.5 flex flex-wrap items-center gap-1">
          <span class={tag} title="What will actually run">
            effective {resolution.effective}
          </span>
          <span class={tag} title="What entered resolution">
            requested {resolution.requested ?? 'none'}
          </span>
          <span class={tag} title="Which precedence level supplied it">
            {sourceSentence(resolution.source)}
          </span>
        </div>
        {#if overruled}
          <p class="mt-1.5 text-[11px] text-foundry-amber">
            Requested <code class="font-mono">{resolution.requested}</code>; this delegation will
            actually run <code class="font-mono">{resolution.effective}</code>. Ceremony may be
            raised, never lowered.
          </p>
          {#each resolution.escalations as reason (reason)}
            <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/60">
              {escalationSentence(reason)}
            </p>
          {/each}
        {/if}
      </div>
    {:else}
      <p
        class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40"
      >
        The server reported no resolution for that selection, so none is shown — this pane never
        computes one itself.
      </p>
    {/if}

    <p class="text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      Precedence is per-delegation request, then this agent's
      <code class="font-mono">[subagents] default_style</code>
      ({data.config_default ?? 'not declared'}), then the built-in default
      ({data.built_in_default}). This lane additionally enforces a floor of
      <code class="font-mono">{data.lane_floor}</code>. Read-only here; edit
      <code class="font-mono">[subagents]</code> in <code class="font-mono">agent.toml</code>.
    </p>
  {/if}
</section>
