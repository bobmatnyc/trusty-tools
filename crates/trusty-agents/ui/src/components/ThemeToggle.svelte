<script lang="ts">
  // One persisted theme preference, presented as a compact dropdown.
  import { theme, setTheme, type Theme } from '../stores/theme';
</script>

<select
  aria-label="Theme"
  value={$theme}
  on:change={(event) => setTheme(event.currentTarget.value as Theme)}
  class="h-8 rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-2 text-xs text-foundry-light-text dark:text-foundry-text focus:outline-none focus:ring-2 focus:ring-foundry-light-primary dark:focus:ring-foundry-primary"
>
  <option value="light">Light</option>
  <option value="system">System</option>
  <option value="dark">Dark</option>
</select>
