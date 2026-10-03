/*
 * Why: this directory vendors the shared code/diff view components from
 * docs/design/UI/design-system/components/. They are copied, not imported,
 * because this crate publishes `ui/src` and builds it with pnpm at install
 * time, and a crates.io tarball cannot carry a file from outside the crate.
 * A copy that drifts from the canonical source is a silent fork.
 * What: compares every vendored file byte-for-byte with its canonical twin,
 * read through Vite's `?raw` loader.
 * Test: this file.
 */
import { describe, expect, it } from 'vitest';
import codeViewJs from './codeView.js?raw';
import codeViewSvelte from './CodeView.svelte?raw';
import diffViewSvelte from './DiffView.svelte?raw';
import canonicalCodeViewJs from '../../../../../../docs/design/UI/design-system/components/codeView.js?raw';
import canonicalCodeViewSvelte from '../../../../../../docs/design/UI/design-system/components/CodeView.svelte?raw';
import canonicalDiffViewSvelte from '../../../../../../docs/design/UI/design-system/components/DiffView.svelte?raw';

describe('vendored Foundry components', () => {
  it.each([
    ['codeView.js', codeViewJs, canonicalCodeViewJs],
    ['CodeView.svelte', codeViewSvelte, canonicalCodeViewSvelte],
    ['DiffView.svelte', diffViewSvelte, canonicalDiffViewSvelte],
  ])('%s matches the canonical source', (_name, vendored, canonical) => {
    expect(canonical.length).toBeGreaterThan(0);
    expect(vendored).toBe(canonical);
  });
});
