<script lang="ts">
  /**
   * Why: Action icons appear inline with role labels, tool calls, and event
   * markers throughout Foundry chat/console surfaces. A single component
   * keyed by name keeps stroke width / viewBox / line caps consistent across
   * the set and avoids importing a heavyweight icon library for a fixed
   * vocabulary. VENDORED COPY (#6439) — the CANONICAL source is
   * docs/design/UI/design-system/icons/ActionIcon.svelte (repo root); see
   * that file's header and docs/design/UI/design-system/icons/README.md for
   * the vendoring pattern and the full name → glyph vocabulary. Fix a bug or
   * add a glyph in the canonical file first, then propagate here and to its
   * ui-memory/ui-analyze siblings until #3492 (`@trusty/foundry` package)
   * replaces copy-paste distribution with a real import.
   * What: Renders one of a fixed set of 24x24 stroke-based SVG icons by
   * name. Falls back to an empty <svg> for unknown names so callers never
   * crash. Stroke color defaults to currentColor so icons inherit text color
   * and react to theme automatically (no light/dark-specific markup needed).
   * Test: <ActionIcon name="pm" /> and <ActionIcon name="agent" /> render
   * distinct shapes; <ActionIcon name="bogus" /> renders nothing visible.
   */
  export let name: string;
  export let size: number = 16;
  export let color: string = 'currentColor';
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke={color}
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  role="img"
>
  {#if name === 'pm'}
    <!-- Simplified robot face: rounded square + 2 eyes + underscore mouth -->
    <rect x="4" y="5" width="16" height="14" rx="3" />
    <circle cx="9" cy="11" r="1" fill={color} stroke="none" />
    <circle cx="15" cy="11" r="1" fill={color} stroke="none" />
    <line x1="9" y1="15" x2="15" y2="15" />
    <line x1="12" y1="5" x2="12" y2="3" />
  {:else if name === 'delegate'}
    <!-- Arrow splitting into two branches -->
    <line x1="12" y1="20" x2="12" y2="13" />
    <path d="M12 13 C 12 9, 7 9, 7 5" />
    <path d="M12 13 C 12 9, 17 9, 17 5" />
    <polyline points="5,7 7,5 9,7" />
    <polyline points="15,7 17,5 19,7" />
  {:else if name === 'agent'}
    <!-- Central node with 3 radiating dots -->
    <circle cx="12" cy="12" r="3" />
    <circle cx="12" cy="4" r="1" fill={color} stroke="none" />
    <circle cx="19" cy="16" r="1" fill={color} stroke="none" />
    <circle cx="5" cy="16" r="1" fill={color} stroke="none" />
    <line x1="12" y1="9" x2="12" y2="5" stroke-dasharray="2 2" />
    <line x1="14.5" y1="13.5" x2="18" y2="15.5" stroke-dasharray="2 2" />
    <line x1="9.5" y1="13.5" x2="6" y2="15.5" stroke-dasharray="2 2" />
  {:else if name === 'workflow'}
    <!-- Three connected boxes: pipeline -->
    <rect x="2" y="9" width="5" height="6" rx="1" />
    <rect x="9.5" y="9" width="5" height="6" rx="1" />
    <rect x="17" y="9" width="5" height="6" rx="1" />
    <line x1="7" y1="12" x2="9.5" y2="12" />
    <line x1="14.5" y1="12" x2="17" y2="12" />
  {:else if name === 'terminal'}
    <!-- Rectangle with `>_` -->
    <rect x="3" y="5" width="18" height="14" rx="2" />
    <polyline points="7,10 9,12 7,14" />
    <line x1="11" y1="14" x2="15" y2="14" />
  {:else if name === 'read_file'}
    <!-- Document with eye overlay -->
    <path d="M6 3 H14 L18 7 V14 H6 Z" />
    <polyline points="14,3 14,7 18,7" />
    <path d="M8 18 C 9.5 16, 14.5 16, 16 18 C 14.5 20, 9.5 20, 8 18 Z" />
    <circle cx="12" cy="18" r="1" fill={color} stroke="none" />
  {:else if name === 'write_file'}
    <!-- Document with pencil -->
    <path d="M5 3 H13 L17 7 V13 H5 Z" />
    <polyline points="13,3 13,7 17,7" />
    <path d="M14 16 L20 10 L22 12 L16 18 L13 19 L14 16 Z" />
  {:else if name === 'web_search'}
    <!-- Magnifying glass with globe -->
    <circle cx="10" cy="10" r="6" />
    <line x1="10" y1="4" x2="10" y2="16" />
    <path d="M4 10 C 7 8, 13 8, 16 10" />
    <path d="M4 10 C 7 12, 13 12, 16 10" />
    <line x1="14.5" y1="14.5" x2="20" y2="20" />
  {:else if name === 'load_skill'}
    <!-- Lightning bolt with download arrow -->
    <polyline points="11,3 6,13 11,13 9,21 16,10 11,10 13,3 11,3" />
    <line x1="18" y1="16" x2="18" y2="21" />
    <polyline points="16,19 18,21 20,19" />
  {:else if name === 'review'}
    <!-- Document with checkmark -->
    <path d="M6 3 H14 L18 7 V21 H6 Z" />
    <polyline points="14,3 14,7 18,7" />
    <polyline points="9,14 11,16 15,12" />
  {:else if name === 'home'}
    <!-- #6439: simple house — an app's own Dashboard/overview nav entry. -->
    <path d="M4 11 L12 4 L20 11" />
    <path d="M6 10 V20 H18 V10" />
  {:else if name === 'indexes'}
    <!-- #6439: stacked disks — a data-index / storage nav entry. -->
    <ellipse cx="12" cy="6" rx="8" ry="2.5" />
    <path d="M4 6 V12 C4 13.5 7.5 14.5 12 14.5 C16.5 14.5 20 13.5 20 12 V6" />
    <path d="M4 12 V18 C4 19.5 7.5 20.5 12 20.5 C16.5 20.5 20 19.5 20 18 V12" />
  {:else if name === 'health'}
    <!-- #6439: monitor with a pulse line — daemon/service health nav entry. -->
    <rect x="3" y="6" width="18" height="12" rx="2" />
    <polyline points="6,12 9,12 10.5,8 13,16 14.5,12 18,12" />
  {:else if name === 'config'}
    <!-- #6439: gear — settings/config nav entry. -->
    <circle cx="12" cy="12" r="3" />
    <path
      d="M12 3 V6 M12 18 V21 M3 12 H6 M18 12 H21 M5.6 5.6 L7.8 7.8 M16.2 16.2 L18.4 18.4 M5.6 18.4 L7.8 16.2 M16.2 7.8 L18.4 5.6"
    />
  {:else if name === 'palaces'}
    <!-- #6439: four-room floor plan — trusty-memory's Palaces nav entry. -->
    <rect x="3" y="3" width="18" height="18" rx="1" />
    <line x1="12" y1="3" x2="12" y2="21" />
    <line x1="3" y1="12" x2="21" y2="12" />
  {:else if name === 'dream'}
    <!-- #6439: crescent moon — trusty-memory's Dream (consolidation) nav entry. -->
    <path
      d="M15 3 C10 3 6 7 6 12 C6 17 10 21 15 21 C11.5 19 9 15.8 9 12 C9 8.2 11.5 5 15 3 Z"
    />
  {:else if name === 'smells'}
    <!-- #6439: warning triangle — trusty-analyze's Smells nav entry. -->
    <path d="M12 4 L21 20 H3 Z" />
    <line x1="12" y1="10" x2="12" y2="14" />
    <circle cx="12" cy="17" r="0.5" fill={color} stroke="none" />
  {:else if name === 'clusters'}
    <!-- #6439: three overlapping circles — trusty-analyze's Clusters nav entry. -->
    <circle cx="9" cy="9" r="5" />
    <circle cx="15" cy="9" r="5" />
    <circle cx="12" cy="15" r="5" />
  {/if}
</svg>
