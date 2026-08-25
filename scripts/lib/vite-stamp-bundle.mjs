// vite-stamp-bundle.mjs — re-write ui-source-hash.txt after Vite empties the
// bundle directory (#5936).
//
// Why: `build.emptyOutDir: true` deletes everything under `dist/`, and the
//   freshness stamp lives inside the bundle so it travels with the packaged
//   tarball (see `ui_stamp_path` in scripts/lib/ui_source_digest.sh). Every
//   `pnpm build` therefore deletes a tracked file, and the deletion lands in
//   whatever commit follows — it reached `main` once through PR #5819 and
//   recurred three more times on 2026-08-18.
//
//   `build.rs` already calls `scripts/stamp-ui-bundle.sh` after a build it ran
//   itself (#6060), but that path cannot recover this deletion. The #5078
//   guard it consults is `check-ui-bundle-freshness.sh`, which reads the stamp
//   from disk when the file is there and falls back to `git show HEAD:<stamp>`
//   when it is not — so a deleted stamp is answered from HEAD's copy, the
//   bundle reports fresh, build.rs returns early, and the re-stamp never fires.
//   Stamping here instead puts the repair at the step that does the damage —
//   the same shape as
//   `crates/trusty-search/Makefile`'s `sync-ui`, where the mirror that wipes
//   `ui-dist/` is followed immediately by the stamp.
//
// What: a build-only Vite plugin whose `closeBundle` hook shells out to
//   `scripts/stamp-ui-bundle.sh <crate>`. `closeBundle` runs once, after every
//   output file is on disk, and only when a build actually produced a bundle —
//   which is exactly the precondition `stamp-ui-bundle.sh` documents, since a
//   stamp written without a rebuild is a claim the publish gate cannot
//   re-verify. A missing script (an extracted crate tarball has no `scripts/`)
//   is a silent no-op; a failing one warns and lets the build finish, matching
//   `restamp_bundle`'s behaviour in build.rs.
//
// Test: scripts/check-ui-bundle-freshness-selftest.sh case 23 runs a real
//   `pnpm build` in each of the three UI projects and asserts the stamp is
//   present and unchanged afterward.

import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Build a Vite plugin that re-stamps `<outDir>/ui-source-hash.txt` after the
 * build writes its output.
 *
 * @param {string} crate Crate directory name under `crates/`, as it appears in
 *   the first column of `scripts/ui-bundle-manifest.tsv`.
 */
export function stampUiBundle(crate) {
  let uiRoot = process.cwd();
  return {
    name: 'trusty:stamp-ui-bundle',
    apply: 'build',
    configResolved(config) {
      // config.root is the UI project directory (crates/<crate>/ui), which is
      // the one path that is stable regardless of the invoking CWD. Deriving
      // the repo root from `import.meta.url` would not be: Vite bundles
      // vite.config.js into a temp file inside the UI directory, which inlines
      // this module and rewrites that URL.
      uiRoot = config.root;
    },
    closeBundle() {
      const repoRoot = resolve(uiRoot, '../../..');
      const script = resolve(repoRoot, 'scripts/stamp-ui-bundle.sh');
      if (!existsSync(script)) {
        return;
      }
      try {
        execFileSync('bash', [script, crate], {
          cwd: repoRoot,
          stdio: ['ignore', 'ignore', 'inherit'],
        });
      } catch {
        console.warn(
          `[trusty:stamp-ui-bundle] ${crate}: the bundle was rebuilt but ` +
            'stamp-ui-bundle.sh failed — run it by hand before committing.',
        );
      }
    },
  };
}
