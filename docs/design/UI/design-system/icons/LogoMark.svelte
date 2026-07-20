<script lang="ts">
  /**
   * Why: The logo mark (robot + "Trusty Assistant" wordmark) is the canonical
   * product lockup used in sidebar headers across trusty-* UIs. Bundling
   * icon + wordmark together guarantees consistent spacing and weight
   * everywhere it appears. This file is the CANONICAL source for the logo
   * lockup — see ../icons/README.md. Crate copies (e.g.
   * crates/trusty-agents/ui/src/lib/icons/LogoMark.svelte) must be kept in
   * sync with this file until #3492 (`@trusty/foundry` package) replaces
   * copy-paste distribution with a real import.
   * What: Renders the RobotIcon (28px by default) inline with the wordmark
   * "Trusty" (regular) + "Assistant" (semibold), both in the Foundry text
   * color, using the Chakra Petch display face. Styled with plain inline
   * CSS custom-property references (design-system/tokens.css's
   * `--trusty-text-primary` / `--trusty-display`) rather than Tailwind
   * utility classes, so this component has zero build-tool dependency and
   * drops into any Svelte app that has loaded tokens.css — consuming crates
   * remain free to re-express the same lockup against their own Tailwind
   * token layer (see crates/trusty-agents/ui's `text-foundry-text` /
   * `font-display` classes for that pattern).
   * Test: Mount <LogoMark /> and visually confirm "Assistant" appears bolder
   * than "Trusty" and the robot icon is left-aligned with the text baseline.
   */
  import RobotIcon from './RobotIcon.svelte';

  export let size: number = 28;
</script>

<span class="logomark">
  <RobotIcon {size} variant="mono" color="var(--trusty-primary, currentColor)" />
  <span class="logomark-word">
    <span class="logomark-word-regular">Trusty </span><span class="logomark-word-semibold"
      >Assistant</span
    >
  </span>
</span>

<style>
  .logomark {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--trusty-text-primary, currentColor);
    font-family: var(--trusty-display, 'Chakra Petch', 'IBM Plex Sans', sans-serif);
  }
  .logomark-word {
    font-size: 0.875rem;
  }
  .logomark-word-regular {
    font-weight: 400;
  }
  .logomark-word-semibold {
    font-weight: 600;
  }
</style>
