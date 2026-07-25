<script lang="ts">
  /**
   * Why (#3932, DOC-57 §3 / §8.4): section 1 of the five-section agent config
   * — "who the agent is". Split out of `AgentConfigPanel` so the shell keeps
   * only tabs, header and the exit guard, per §8.4's decomposition.
   *
   * Two things this component deliberately does NOT own:
   * (1) The persona TEXT. The shell owns `content` and binds it in, because
   *     switching sections unmounts this component and a child-owned buffer
   *     would silently drop a half-written system prompt on every tab click —
   *     and would also strand the shell's `configPaneDirty` aggregation, which
   *     every exit path (Esc, Back, Close, Chat→Events) depends on (§8.4, G-6a).
   * (2) Saving. `onSave` calls back into the shell, which holds the one PATCH
   *     path and the confirm-before-discard dialog.
   *
   * What: The full-height `persona.md` editor from #3895, unchanged in
   * behavior and in its sizing invariants, plus an honest statement about
   * per-connector event instructions. That block used to enumerate
   * `DEFINED_LISTENERS` and assert "no event instructions configured" for each
   * — a fabrication (§3.2, P-2: the block MUST be backed by the real
   * `events/*.md` file set) and one no route can back in this phase, so it is
   * replaced by a statement of the gap rather than a guess about its contents.
   * Test: `AgentConfigPersonality.test.ts`; the editor's sizing invariants stay
   * pinned by `AgentConfigPanel.test.ts` and `tests/config-takeover.spec.ts`.
   */
  import { Save } from 'lucide-svelte';

  /** `false` for flat (non-package) agents — no `persona.md` to write. */
  export let personaEditable = false;
  /** Bound by the shell, which owns the edit buffer across section switches. */
  export let content = '';
  export let dirty = false;
  export let saving = false;
  export let justSaved = false;
  export let onSave: () => void;
</script>

<div class="flex min-h-0 flex-1 flex-col gap-2">
  <p class="shrink-0 text-xs text-foundry-light-muted dark:text-foundry-text/60">
    Main instructions — this agent's <code class="font-mono">persona.md</code>.
  </p>
  {#if !personaEditable}
    <p class="shrink-0 rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/50">
      This agent has no editable persona.md (flat-file agents don't carry a separate personality
      file).
    </p>
  {:else}
    <!-- #3894 (supersedes #3862's vh clamp): the instructions editor is the
         primary surface of this pane, so it takes EVERY remaining pixel of the
         takeover — `flex-1 min-h-0`, no `rows`, no `min-h-[55vh]/max-h-[70vh]`
         clamp (which capped it at 70% of the viewport and left dead space
         below on a tall window, and could overflow its container on a short
         one). It is the only scroll container in this tab; `resize-none`
         because a drag handle can only make a full-height field smaller. -->
    <textarea
      bind:value={content}
      spellcheck="false"
      data-instructions-editor
      class="w-full min-h-0 flex-1 resize-none overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-3 py-2 font-mono text-xs leading-relaxed text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
    ></textarea>
    <div class="flex shrink-0 items-center gap-2 pt-1">
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1.5 text-xs font-semibold text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-50"
        disabled={saving || !dirty}
        on:click={onSave}
      >
        <Save class="h-3.5 w-3.5" /> {saving ? 'Saving…' : 'Save'}
      </button>
      {#if justSaved}
        <span class="text-xs text-foundry-teal">Saved</span>
      {/if}
    </div>
  {/if}
  <p class="shrink-0 pt-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
    Per-connector event instructions (<code class="font-mono">events/&lt;connector&gt;.md</code>) are
    loaded at wake time, but no route lists which files an agent carries — so this pane cannot show
    them and does not guess (DOC-57 §3.2, P-2).
  </p>
</div>
