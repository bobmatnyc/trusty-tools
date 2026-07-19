<script lang="ts">
  // Why: DOC-39 §2's "two nested scopes" framing (issue #3153 shell rebuild)
  // — the header is the OUTER "app context" scope: branding + the
  // workstream switcher, always present, rendering an empty or hydrated
  // state (§4.4 AC-4.1). Phase 1 has no workstream registry yet (§6.3/7B —
  // "single synthetic current session entry, no registry"), so the
  // workstream box is a READ-ONLY label rather than a real switcher.
  //
  // DOC-48 reservation (PR #3284, tcode workstreams spec): a NEW spec
  // reserves a workstream-switcher control slot in this exact header
  // position per DOC-39 §2/§8. This component must not preclude that slot —
  // it renders a disabled placeholder control next to the read-only label
  // rather than omitting the affordance outright, so DOC-48's follow-up
  // only needs to enable it, not carve out header space from scratch.
  //
  // Tokens/cost on the right is a compact GLOBAL readout (distinct from
  // `StatusBar.svelte`'s per-session TOKENS/COST segments) — both are stub
  // text today since live cost tracking is issue #3254, not yet shipped.
  //
  // What: A `.hdr` flex row: brand mark + name on the left, the read-only
  // workstream box + reserved switcher slot in the middle, tokens/cost stub
  // + a disabled settings control on the right.
  // Test: `App.test.ts` pins `.hdr` renders inside `.app` as the shell's
  // first child (structural smoke coverage) — no dedicated
  // `AppHeader.test.ts` exists yet since every value here is static Phase 1
  // stub content, nothing pure/branchy to unit-test independently.
</script>

<header
  class="hdr flex h-14 shrink-0 items-center justify-between gap-4 border-b border-trusty-border bg-trusty-raised px-4"
>
  <div class="flex items-center gap-2">
    <span class="font-display text-sm font-bold tracking-wide text-trusty-primary">◆</span>
    <span class="font-display text-sm font-bold uppercase tracking-wide text-trusty-text">
      Trusty Code
    </span>
  </div>

  <div class="flex min-w-0 flex-1 items-center justify-center gap-1.5">
    <span
      class="truncate rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-2.5 py-1 font-mono text-[11px] uppercase tracking-wide text-trusty-text-secondary"
      title="Workstream switcher lands with DOC-48 (PR #3284) — Phase 1 shows the current workstream as a read-only label."
    >
      workstream: current session
    </span>
    <button
      type="button"
      disabled
      aria-label="switch workstream"
      title="Workstream switcher — reserved slot, DOC-48 (PR #3284), not yet built"
      class="rounded-sm border-1.5 border-trusty-border bg-trusty-card px-1.5 py-1 font-mono text-[10px] text-trusty-text-muted disabled:cursor-not-allowed disabled:opacity-50"
    >
      ▾
    </button>
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
