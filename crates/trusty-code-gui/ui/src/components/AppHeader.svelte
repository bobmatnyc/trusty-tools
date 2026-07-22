<script lang="ts">
  // Why: DOC-39 §2's "two nested scopes" framing (issue #3153 shell rebuild)
  // — the header is the OUTER "app context" scope: branding + the
  // workstream switcher, always present, rendering an empty or hydrated
  // state (§4.4 AC-4.1). Phase 1 shipped a READ-ONLY label + disabled `▾`
  // placeholder here because "Phase 1 has no workstream registry yet"; DOC-48
  // (epic #3292) built that registry, and its Phase C — issue #3300 — is the
  // real switcher now mounted in the exact slot the placeholder reserved:
  // `WorkstreamSwitcher.svelte` (state display, activation, close, rename;
  // see its own module docs for the full design).
  //
  // Tokens/cost on the right is a compact GLOBAL readout (distinct from
  // `StatusBar.svelte`'s per-session TOKENS/COST segments) — both are stub
  // text today since live cost tracking is issue #3254, not yet shipped.
  //
  // What: A `.hdr` flex row: brand mark + name on the left, `WorkstreamSwitcher`
  // in the middle, tokens/cost stub + a disabled settings control on the
  // right.
  // Test: `App.test.ts` pins `.hdr` renders inside `.app` as the shell's
  // first child (structural smoke coverage); `WorkstreamSwitcher.test.ts`
  // covers the switcher itself — this component has no branchy logic of its
  // own to test independently.
  import WorkstreamSwitcher from './WorkstreamSwitcher.svelte';
  import RobotIcon from '../lib/icons/RobotIcon.svelte';
</script>

<header
  class="hdr flex h-14 shrink-0 items-center justify-between gap-4 border-b border-trusty-border bg-trusty-raised px-4"
>
  <div class="flex items-center gap-2">
    <span class="flex items-center text-trusty-primary">
      <RobotIcon variant="mono" size={18} color="currentColor" />
    </span>
    <span class="font-display text-sm font-bold uppercase tracking-wide text-trusty-text">
      Trusty Code
    </span>
  </div>

  <div class="flex min-w-0 flex-1 items-center justify-center gap-1.5">
    <WorkstreamSwitcher />
  </div>

  <div class="flex items-center gap-3">
    <span
      class="font-mono text-[11px] uppercase tracking-wide text-trusty-text-muted"
      title="Global tokens/cost readout — live cost tracking is issue #3254, not yet shipped"
    >
      tokens: — · cost: —
    </span>
    <button
      type="button"
      disabled
      aria-label="settings"
      title="Settings — not yet implemented"
      class="rounded-sm border-1.5 border-trusty-border bg-trusty-card px-1.5 py-1 font-mono text-[11px] text-trusty-text-muted disabled:cursor-not-allowed disabled:opacity-50"
    >
      ⚙
    </button>
  </div>
</header>
