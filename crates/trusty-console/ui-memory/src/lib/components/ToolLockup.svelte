<script lang="ts">
  /**
   * VENDORED COPY (#7589) — the CANONICAL source is
   * docs/design/UI/design-system/icons/ToolLockup.svelte (repo root). Fix it
   * there first, then propagate to this copy and its
   * ui-search/ui-memory/ui-analyze siblings, per that directory’s README.md,
   * until #3492 (`@trusty/foundry` package) replaces copy-paste distribution
   * with a real import.
   * Why: `LogoMark.svelte` hardcodes the "Trusty Assistant" wordmark, so a
   * tool whose own name belongs beside the robot — a service dashboard's
   * Topbar, for instance — had no Foundry lockup to reach for and drew its
   * own. #7589 is that gap: three dashboards carried a bare breadcrumb row
   * with no brand mark at all. This is the same lockup with the wordmark
   * supplied by the caller, so every tool reads as one product family
   * without a per-crate re-drawing. CANONICAL source — see
   * ../icons/README.md for the vendoring rules; fix it here first.
   * What: Renders `RobotIcon` in its `mono` variant beside the tool name.
   * Colour and type come from Foundry tokens only (`--trusty-primary`
   * falling back to `--trusty-accent`, `--trusty-text-primary`,
   * `--trusty-display`), so the lockup inverts with `data-theme` exactly
   * like the rest of a Foundry surface and introduces no new hue. The
   * two-step accent fallback exists because not every consuming token file
   * defines `--trusty-primary` yet.
   * Test: Mount `<ToolLockup name="Trusty Search" />` under
   * `<html data-theme="light">` and again under `dark`; the mark must take
   * the rust accent of each palette and the wordmark the primary text
   * colour. Mounted-in-Topbar coverage lives in each dashboard's
   * `Topbar.test.js`.
   */
  import RobotIcon from './RobotIcon.svelte';

  export let name: string;
  export let size: number = 22;
</script>

<span class="tool-lockup">
  <RobotIcon
    {size}
    variant="mono"
    color="var(--trusty-primary, var(--trusty-accent, currentColor))"
  />
  <span class="tool-lockup-name">{name}</span>
</span>

<style>
  .tool-lockup {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
    color: var(--trusty-text-primary, currentColor);
    font-family: var(--trusty-display, 'Chakra Petch', 'IBM Plex Sans', sans-serif);
  }
  .tool-lockup-name {
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    white-space: nowrap;
  }
</style>
