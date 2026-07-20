<script lang="ts">
  // Why: While the coordinator works inside a session the user wants a quick
  // glance at which files are being touched, without leaving the chat panel.
  // What: A sidebar pane that polls the active session's tmux `output` every
  // few seconds, scrapes file-path-looking tokens, and lists them with an
  // extension-appropriate icon. Stub-grade: regex scraping, no daemon API yet.
  // Test: With an active session whose output mentions `src/foo.rs`, that
  // path appears in the list; with `$activeSessionId` null, the empty state
  // ("No active session") renders.
  import { onDestroy } from 'svelte';
  import { File, FileCode, FileText, FileJson } from 'lucide-svelte';
  import { invoke } from '../lib/transport';
  import { activeSessionId } from '../stores/app';

  /** Poll interval for the session output scrape, in ms. */
  const POLL_MS = 5000;

  /** Distinct file paths most recently seen in the tmux output. */
  let files: string[] = [];

  let timer: ReturnType<typeof setInterval> | undefined;

  /** File extensions treated as source code (FileCode icon). */
  const CODE_EXT = new Set([
    'rs', 'ts', 'tsx', 'js', 'jsx', 'svelte', 'py', 'go', 'java',
    'c', 'cpp', 'h', 'sh', 'rb', 'css', 'html',
  ]);

  /** File extensions treated as plain text/docs (FileText icon). */
  const TEXT_EXT = new Set(['md', 'txt', 'log', 'toml', 'yaml', 'yml', 'cfg']);

  /**
   * Why: The icon should hint at the file kind so the list scans quickly.
   * What: Returns a lucide icon component based on the path's extension.
   * Test: `iconFor('a.rs')` returns `FileCode`; `iconFor('a.json')` returns
   * `FileJson`; an unknown extension returns the generic `File`.
   */
  function iconFor(path: string): typeof File {
    const ext = path.split('.').pop()?.toLowerCase() ?? '';
    if (ext === 'json') return FileJson;
    if (CODE_EXT.has(ext)) return FileCode;
    if (TEXT_EXT.has(ext)) return FileText;
    return File;
  }

  /**
   * Why: tmux output is free-form text; we approximate "modified files" by
   * scraping anything that looks like a path with an extension.
   * What: Matches `\S+\.\w+` tokens, de-duplicates, and keeps the last 30.
   * Test: Given "wrote src/a.rs and src/b.ts", returns both paths.
   */
  function scrape(text: string): string[] {
    const matches = text.match(/\S+\.\w+/g) ?? [];
    const seen = new Set<string>();
    const out: string[] = [];
    for (const m of matches) {
      const path = m.replace(/[),.;:'"]+$/, '');
      if (path.includes('.') && !seen.has(path)) {
        seen.add(path);
        out.push(path);
      }
    }
    return out.slice(-30).reverse();
  }

  /**
   * Why: Each poll tick refreshes the list from the active session's pane.
   * What: Invokes `session_output` for `$activeSessionId`, normalizes the
   * payload to a string, and scrapes it; clears the list on any failure.
   * Test: With the daemon returning pane text, `files` is populated; with the
   * call failing, `files` is emptied.
   */
  async function poll(): Promise<void> {
    const id = $activeSessionId;
    if (!id) {
      files = [];
      return;
    }
    try {
      const raw = await invoke('session_output', { id });
      const text =
        typeof raw === 'string'
          ? raw
          : (raw?.output ?? raw?.text ?? JSON.stringify(raw));
      files = scrape(text);
    } catch {
      files = [];
    }
  }

  // Restart the poll loop whenever the active session changes.
  $: {
    if (timer) clearInterval(timer);
    if ($activeSessionId) {
      poll();
      timer = setInterval(poll, POLL_MS);
    } else {
      files = [];
    }
  }

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  /** Last path segment, for a compact label. */
  function basename(path: string): string {
    const parts = path.split('/');
    return parts[parts.length - 1] || path;
  }
</script>

{#if !$activeSessionId}
  <p class="px-3 py-4 text-xs opacity-60">
    No active session — select one to track its files.
  </p>
{:else if files.length === 0}
  <p class="px-3 py-4 text-xs opacity-60">No files seen yet.</p>
{:else}
  <ul class="py-1">
    {#each files as path (path)}
      <li
        class="flex items-center gap-2 px-3 py-1.5 text-xs hover:bg-trusty-border/60"
        title={path}
      >
        <svelte:component
          this={iconFor(path)}
          size={13}
          class="shrink-0 opacity-60"
        />
        <span class="truncate font-mono">{basename(path)}</span>
      </li>
    {/each}
  </ul>
{/if}
