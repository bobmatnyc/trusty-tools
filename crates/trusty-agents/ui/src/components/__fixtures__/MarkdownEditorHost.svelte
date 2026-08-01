<script lang="ts">
  /**
   * Why: `MarkdownEditor` is a CONTROLLED component — the behaviours that
   * matter (document tracks an externally changed `value`, `readonly` flips a
   * live view) only exist when a parent changes a prop after mount. Svelte 5's
   * `mount()` takes a plain props object that is not reactive, so a test cannot
   * drive those paths by assigning to the returned instance; it needs a real
   * parent whose state is reactive. This is that parent, and it exists only for
   * `MarkdownEditor.test.ts`.
   * What: holds `value`/`readonly` in `$state` and exposes setters. Runes are
   * used here (unlike the legacy-syntax components around it) precisely because
   * reactive local state is the whole point of the fixture.
   * Test: used by `MarkdownEditor.test.ts`.
   */
  import { untrack } from 'svelte';
  import MarkdownEditor from '../MarkdownEditor.svelte';

  const props: { initialValue?: string; initialReadonly?: boolean } = $props();

  // Seed only — the setters below own these from here on, so the initial read
  // is deliberately untracked.
  let value = $state(untrack(() => props.initialValue ?? ''));
  let readonly = $state(untrack(() => props.initialReadonly ?? false));

  export function setValue(next: string) {
    value = next;
  }

  export function setReadonly(next: boolean) {
    readonly = next;
  }
</script>

<MarkdownEditor {value} {readonly} />
