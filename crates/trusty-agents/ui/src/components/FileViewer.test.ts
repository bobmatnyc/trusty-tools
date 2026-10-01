/*
 * Why: the source and diff blocks of this viewer now come from the shared
 * Foundry components (`lib/foundry/`, canonical in
 * docs/design/UI/design-system/components/). These tests pin the DOM the
 * viewer rendered before that move, so the agents GUI behaves the same.
 * What: mounts the real FileViewer with the workspace-file API mocked at its
 * module boundary, opens a Rust file, and checks the highlighted source block;
 * then switches to the diff and checks the per-line diff classes.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount, tick } from 'svelte';
import { writable } from 'svelte/store';

vi.mock('../lib/workspaceFiles', () => ({ readWorkspaceFile: vi.fn(), diffWorkspaceFile: vi.fn() }));
vi.mock('../stores/workspace', () => ({ openedFile: writable(null) }));

import FileViewer from './FileViewer.svelte';
import { openedFile } from '../stores/workspace';
import { readWorkspaceFile, diffWorkspaceFile } from '../lib/workspaceFiles';

const root = { id: 'r1', name: 'Repo', path: '/repo' };
let component: ReturnType<typeof mount> | undefined;

afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  (openedFile as ReturnType<typeof writable>).set(null);
  document.body.innerHTML = '';
  vi.clearAllMocks();
});

async function settle() {
  for (let i = 0; i < 4; i++) { await Promise.resolve(); await tick(); }
  flushSync();
}

async function open(path: string, content: string) {
  vi.mocked(readWorkspaceFile).mockResolvedValue({ path, kind: 'code', content, size: content.length });
  (openedFile as ReturnType<typeof writable>).set({ root, path });
  component = mount(FileViewer, { target: document.body });
  await settle();
}

describe('FileViewer with the shared code and diff views', () => {
  it('renders a code file as a highlighted file-code block with no gutter', async () => {
    await open('src/main.rs', 'fn main() {}\n');
    const pre = document.querySelector('pre[aria-label="File source"]') as HTMLPreElement;
    expect(pre).not.toBeNull();
    expect(pre.classList.contains('file-code')).toBe(true);
    expect(pre.classList.contains('text-sm')).toBe(true);
    expect(pre.querySelector('code .hljs-keyword')?.textContent).toBe('fn');
    expect(document.querySelector('.gutter')).toBeNull();
  });

  it('renders the diff with one classified line per diff line', async () => {
    vi.mocked(diffWorkspaceFile).mockResolvedValue({
      available: true,
      diff: '--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn main() {}',
    });
    await open('src/main.rs', 'fn main() {}\n');
    (Array.from(document.querySelectorAll('button')).find((b) => b.textContent === 'Diff') as HTMLButtonElement).click();
    await settle();
    const pre = document.querySelector('pre[aria-label="File diff"]') as HTMLPreElement;
    expect(pre.classList.contains('font-mono')).toBe(true);
    const lines = Array.from(pre.querySelectorAll('div'));
    expect(lines.map((l) => l.textContent)).toEqual(['--- a/src/main.rs', '+++ b/src/main.rs', '@@ -1 +1 @@', '-fn old() {}', '+fn main() {}']);
    expect(lines.map((l) => ['diff-added', 'diff-removed', 'diff-hunk'].find((c) => l.classList.contains(c)) ?? '')).toEqual(['', '', 'diff-hunk', 'diff-removed', 'diff-added']);
    expect(diffWorkspaceFile).toHaveBeenCalledWith('r1', 'src/main.rs');
  });
});
