<script lang="ts">
  /**
   * Why (brand identity wiring): constrained spots (sidebar header, compact
   * badges) need the standalone Foundry UNIT robot mark rather than the full
   * horizontal lockup (see `docs/design/UI/icons/README.md` — the mark is
   * "for constrained spaces"). Previously these spots used the generic,
   * pre-identity-suite `RobotIcon` component; this wires in the actual
   * brand mark asset instead.
   * What: Inlines `docs/design/UI/icons/trusty-agents-mark.svg` verbatim
   * (fixed rust-orange fill, `viewBox="0 0 64 64"`) at the requested `size`.
   * Unlike the full lockup (`Logo.svelte`), the mark ships a single
   * treatment — no reversed variant — because its rust-orange-on-transparent
   * rendering already reads correctly on both paper and Night Shift oxide
   * fields (the same reasoning the prior `RobotIcon` "mono" variant relied
   * on). Per the identity README: do not recolor, add shadows, or place the
   * mark in a circle.
   * Test: Mount `<Mark size={20} />` in both light and dark theme and
   * confirm the rust-orange robot face renders identically (no shadow, no
   * circular clip) in both.
   */
  export let size: number = 20;
</script>

<svg
  width={size}
  height={size}
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 64 64"
  role="img"
  aria-labelledby="trusty-agents-mark-title trusty-agents-mark-desc"
>
  <title id="trusty-agents-mark-title">Trusty Agents robot mark</title>
  <desc id="trusty-agents-mark-desc">Foundry UNIT robot with a calm machine face.</desc>
  <path d="M32 16V8" fill="none" stroke="#B7410E" stroke-width="3" />
  <rect x="29" y="3" width="6" height="6" rx="1.5" fill="#B7410E" />
  <rect x="8" y="16" width="48" height="42" rx="7" fill="#B7410E" />
  <rect x="17" y="28" width="7" height="7" rx="1" fill="#FFFDF9" />
  <rect x="40" y="28" width="7" height="7" rx="1" fill="#FFFDF9" />
  <path d="M21 48h22" fill="none" stroke="#FFFDF9" stroke-width="3" stroke-linecap="square" />
</svg>
