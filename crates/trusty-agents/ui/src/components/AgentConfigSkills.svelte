<script lang="ts">
  /**
   * Why (#3932, DOC-57 §5 / §8.2): section 3 of the five-section agent config
   * — "what the agent can do". Bob: *"Instead of tools, let's show skills…
   * each tool should be wrapped in a skill."* The skill becomes the visible
   * unit of capability; a raw tool name becomes an implementation detail
   * beneath it. This replaces the Tools tab, whose entire body was one
   * monospace `<textarea>` of raw globs with no catalog, no validation and no
   * grouping — G-3 requires cards instead.
   *
   * What: One card per capability family, derived from `[tools].allow` by
   * `synthesizeSkills` (§5.5, S-7/S-8) and badged `synthetic` (S-10), which is
   * the point rather than an apology: no manifest declares `tools:` yet, so
   * badging every card keeps the unwrapped surface VISIBLE and makes wrapping
   * progress measurable as authored manifests land in Phase 2 (#3933).
   *
   * Read-only, and deliberately so: this pane drops the Tools textarea's write
   * path because capability editing returns as `PATCH { skills_allow }` in
   * Phase 3 (§5.7, S-12 — "Phase 1 is read-only"), on the skill names this
   * pane shows rather than on the globs beneath them. Until then `agent.toml`
   * is the edit surface, which the pane says outright.
   *
   * Availability is NOT rendered. §8.3 lists it on the card, but nothing in
   * this phase resolves a pattern against the tool registry, and a green
   * "available" chip nobody checked is exactly the fabrication G-4 forbids.
   * Test: `AgentConfigSkills.test.ts`.
   */
  import type { SyntheticSkill } from '../lib/agentConfig';

  /** Derived by the shell via `synthesizeSkills(detail.tools_allow)`. */
  export let skills: SyntheticSkill[] = [];
</script>

<div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
    What this agent can do. Each card is a capability family wrapping the tools beneath it, grouped
    from the allow-list by tool-name prefix — a heuristic, not a taxonomy, which an authored
    manifest supersedes. Edit the underlying allow-list in
    <code class="font-mono">agent.toml</code>'s <code class="font-mono">[tools].allow</code> —
    editing skills from here arrives with <code class="font-mono">[skills].allow</code> (DOC-57
    §5.7, #3933).
  </p>

  {#if skills.length === 0}
    <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      This agent declares no <code class="font-mono">[tools].allow</code> entries. On the persona
      chat path an absent allow-list grants <strong>no</strong> tools — it is not "unrestricted"
      (DOC-57 §7.1).
    </p>
  {/if}

  {#each skills as skill (skill.name)}
    <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
      <div class="flex items-center justify-between gap-2">
        <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">
          {skill.name}
        </span>
        {#if skill.synthetic}
          <span
            class="shrink-0 rounded-md bg-foundry-amber/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-amber"
            title="Derived from [tools].allow — no skill manifest was authored for this family"
          >
            synthetic
          </span>
        {/if}
      </div>
      <!-- Tool list collapsed by default (§8.3, G-3): the card is the unit the
           user reasons about; the patterns beneath it are the detail. -->
      <details class="mt-1.5">
        <summary class="cursor-pointer font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/40">
          {skill.patterns.length} allow-list {skill.patterns.length === 1 ? 'pattern' : 'patterns'}
        </summary>
        <div class="mt-1.5 flex flex-wrap gap-1">
          {#each skill.patterns as pattern (pattern)}
            <span class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-text dark:text-foundry-text">
              {pattern}
            </span>
          {/each}
        </div>
      </details>
    </div>
  {/each}

  {#if skills.length > 0}
    <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
      Every card is <strong>synthetic</strong>: it was derived from
      <code class="font-mono">[tools].allow</code>, not from an authored skill manifest. Whether each
      wrapped pattern resolves to a live tool is not checked here — no route resolves the registry in
      this phase (DOC-57 §5.7).
    </p>
  {/if}
</div>
