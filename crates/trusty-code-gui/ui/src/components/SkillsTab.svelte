<script lang="ts">
  // Why: issue #3449 — Bob: "add a 'Skills' tab along with 'Agents', and
  // the user should have the ability to add/remove both." A NEW tab (no
  // prior stub existed) — the sibling of `AgentsTab.svelte`, same shape,
  // over `GET`/`POST /skills`, `DELETE /skills/{name}`
  // (`crate::serve::rest::skill_catalog`).
  //
  // **tcode's skill model is narrower than its agent model — reflected
  // here, not papered over.** `crate::skills::protocol` recognises exactly
  // TWO tiers: `"bundled"` (embedded, read-only — trusty-mpm's universal
  // skill set) and `"project"` (`<project_root>/.claude/skills/<name>/
  // SKILL.md`) — there is NO user-level `~/.claude/skills` tier the way
  // agents has `~/.claude/agents` (see that module's docs for why). The add
  // flow is therefore disabled with an explicit reason when the daemon is
  // projectless, rather than silently failing on submit or inventing a
  // tier tcode does not actually consume.
  //
  // Polling/two-step-confirm shape mirrors `AgentsTab.svelte` exactly (see
  // its module doc for the `resetRowState()`/`WorkstreamSwitcher.svelte`
  // rationale) — described once there, not re-derived here.
  //
  // Test: `SkillsTab.test.ts`.
  import { apiBase } from '../lib/api-config';
  import {
    createSkill,
    deleteSkill,
    fetchSkillRoster,
    type SkillCatalogEntry,
  } from '../lib/skill-roster';

  const POLL_MS = 5000;

  type Phase = 'connecting' | 'daemon-unreachable' | 'ready';

  let phase = $state<Phase>('connecting');
  let skills = $state<SkillCatalogEntry[]>([]);
  let error = $state<string | null>(null);
  let hasProject = $state(false);

  let addOpen = $state(false);
  let addName = $state('');
  let addContent = $state('');
  let addBusy = $state(false);
  let addError = $state<string | null>(null);

  let removeConfirmName = $state<string | null>(null);
  let removeBusy = $state(false);
  let removeError = $state<string | null>(null);

  let pollController: AbortController | null = null;

  function msg(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function resetRowState() {
    removeConfirmName = null;
    removeError = null;
  }

  /** `GET /sessions`' active session `project` field is the only
   * daemon-owned "is a project bound" fact available today (same gap
   * `App.svelte`'s own doc names for `isGitRepo`) — reused here purely to
   * decide whether to disable the add flow, never to gate the tab's
   * visibility (that's `nav-tabs.ts::tabVisibility`'s job). */
  async function refreshProjectFlag(base: string, signal: AbortSignal) {
    try {
      const res = await fetch(`${base}/sessions`, { signal });
      if (!res.ok) return;
      const body = (await res.json()) as { sessions: Array<{ project: string | null }> };
      if (signal.aborted) return;
      hasProject = body.sessions.some((s) => s.project != null);
    } catch {
      // Non-fatal — the daemon-reachability state is already tracked by
      // `refresh()`'s own try/catch; this flag just stays at its last value.
    }
  }

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = msg(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const roster = await fetchSkillRoster(base, signal);
      if (signal.aborted) return;
      skills = roster;
      phase = 'ready';
      error = null;
      await refreshProjectFlag(base, signal);
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = msg(e);
      }
    }
  }

  $effect(() => {
    const controller = new AbortController();
    pollController = controller;
    void refresh(controller.signal);
    const timer = setInterval(() => void refresh(controller.signal), POLL_MS);
    return () => {
      controller.abort();
      if (pollController === controller) pollController = null;
      clearInterval(timer);
    };
  });

  function toggleAdd() {
    addOpen = !addOpen;
    addError = null;
    if (!addOpen) {
      addName = '';
      addContent = '';
    }
    resetRowState();
  }

  async function onFilePicked(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    addContent = await file.text();
    if (!addName) {
      addName = file.name.replace(/\.md$/i, '');
    }
  }

  async function submitAdd() {
    if (addBusy || !hasProject || !addName.trim() || !addContent.trim()) return;
    addBusy = true;
    addError = null;
    try {
      const base = await apiBase();
      await createSkill(base, addName.trim(), addContent);
      addOpen = false;
      addName = '';
      addContent = '';
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      addError = msg(e);
    } finally {
      addBusy = false;
    }
  }

  async function doRemove(name: string) {
    if (removeBusy) return;
    removeBusy = true;
    try {
      const base = await apiBase();
      await deleteSkill(base, name);
      removeConfirmName = null;
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      removeError = msg(e);
    } finally {
      removeBusy = false;
    }
  }

  function tierLabel(tier: SkillCatalogEntry['tier']): string {
    return tier === 'bundled' ? 'bundled · read-only' : 'project';
  }
</script>

<section class="rounded border-1.5 border-trusty-border bg-trusty-card">
  <div
    class="flex items-center justify-between border-b border-trusty-border bg-trusty-raised px-4 py-2.5"
  >
    <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">skills</h2>
    <button
      type="button"
      onclick={toggleAdd}
      disabled={!hasProject}
      title={hasProject ? undefined : 'skills are project-scoped — bind a project to add one'}
      class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary disabled:cursor-not-allowed disabled:opacity-50"
    >
      {addOpen ? 'cancel' : '+ add skill'}
    </button>
  </div>

  <div class="p-4">
    {#if !hasProject}
      <p class="mb-3 text-[11px] text-trusty-text-muted">
        skills are project-scoped (no user-level tier) — bind a project to add or remove one; the
        bundled catalog below is always available.
      </p>
    {/if}

    {#if addOpen}
      <div class="mb-4 rounded border-1.5 border-trusty-border bg-trusty-raised p-3">
        <label
          class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
          for="skill-add-name"
        >
          name (lowercase, digits, hyphens)
        </label>
        <input
          id="skill-add-name"
          bind:value={addName}
          placeholder="my-custom-skill"
          class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-1.5 font-mono text-xs text-trusty-text"
        />

        <label
          class="mt-2 block font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
          for="skill-add-content"
        >
          SKILL.md content (Markdown + frontmatter)
        </label>
        <textarea
          id="skill-add-content"
          bind:value={addContent}
          rows="8"
          placeholder={'---\nname: my-custom-skill\ndescription: ...\n---\n\nSkill instructions.'}
          class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-1.5 font-mono text-xs text-trusty-text"
        ></textarea>

        <div class="mt-2 flex items-center gap-2">
          <input
            type="file"
            accept=".md,text/markdown,text/plain"
            onchange={onFilePicked}
            class="font-mono text-[10px] text-trusty-text-muted"
          />
        </div>

        {#if addError}
          <p class="mt-2 text-xs text-status-error">{addError}</p>
        {/if}

        <button
          type="button"
          disabled={addBusy || !addName.trim() || !addContent.trim()}
          onclick={submitAdd}
          class="mt-2 rounded-sm border-1.5 border-trusty-primary bg-trusty-primary px-2.5 py-1 font-mono text-[11px] uppercase tracking-wide text-trusty-surface disabled:cursor-not-allowed disabled:opacity-50"
        >
          {addBusy ? 'creating…' : 'create'}
        </button>
      </div>
    {/if}

    {#if phase === 'connecting'}
      <p class="text-xs text-trusty-text-muted">connecting&hellip;</p>
    {:else if phase === 'daemon-unreachable'}
      <p class="flex items-center gap-1.5 text-xs text-status-error">
        <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
        daemon unreachable{error ? ` — ${error}` : ''}
      </p>
    {:else if skills.length === 0}
      <p class="text-xs text-trusty-text-muted">no skills found</p>
    {:else}
      <ul class="flex flex-col gap-1.5">
        {#each skills as skill (skill.name)}
          <li
            class="flex items-center justify-between gap-2 rounded-sm border-1.5 border-trusty-border px-2.5 py-1.5"
          >
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="truncate font-mono text-xs text-trusty-text">{skill.name}</span>
                <span
                  class="shrink-0 rounded-sm border border-trusty-border-strong px-1 font-mono text-[9px] uppercase tracking-wide text-trusty-text-muted"
                >
                  {tierLabel(skill.tier)}
                </span>
              </div>
              {#if skill.description}
                <p class="truncate text-[11px] text-trusty-text-secondary">{skill.description}</p>
              {/if}
            </div>

            {#if skill.tier !== 'bundled'}
              {#if removeConfirmName === skill.name}
                <div class="flex shrink-0 items-center gap-1.5">
                  <button
                    type="button"
                    class="font-mono text-[10px] uppercase tracking-wide text-status-error"
                    disabled={removeBusy}
                    onclick={() => doRemove(skill.name)}
                  >
                    confirm
                  </button>
                  <button
                    type="button"
                    class="font-mono text-[10px] uppercase tracking-wide text-trusty-text-muted"
                    disabled={removeBusy}
                    onclick={() => (removeConfirmName = null)}
                  >
                    never mind
                  </button>
                </div>
              {:else}
                <button
                  type="button"
                  class="shrink-0 px-1 font-mono text-[11px] text-trusty-text-muted hover:text-status-error"
                  aria-label={`remove ${skill.name}`}
                  onclick={() => (removeConfirmName = skill.name)}
                >
                  ×
                </button>
              {/if}
            {/if}
          </li>
        {/each}
      </ul>
      {#if removeError}
        <p class="mt-2 text-xs text-status-error">{removeError}</p>
      {/if}
    {/if}
  </div>
</section>
