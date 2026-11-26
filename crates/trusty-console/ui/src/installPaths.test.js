/**
 * Tests for the Config tab's install-paths view model (#11821 P5).
 *
 * Run: `node --test src/installPaths.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { installPathsView, sourceLabel } from './installPaths.js';

test('ok state lists both rows with their source and keeps the effective root', () => {
  const v = installPathsView({
    install_file: '/h/.trusty-tools/install.toml',
    projects_root: { path: '/h/proj', source: 'install-file' },
    config_dir: { path: '/h/cfg', source: 'default' },
    error: null,
  });
  assert.equal(v.state, 'ok');
  assert.deepEqual(
    v.rows.map((r) => [r.label, r.path, r.sourceLabel]),
    [
      ['Project home', '/h/proj', 'install file'],
      ['Config dir', '/h/cfg', 'default'],
    ],
  );
  assert.equal(v.showEffectiveRoot, true);
});

test('error state hides the rows and the effective root, and names the file', () => {
  const v = installPathsView({
    install_file: '/h/.trusty-tools/install.toml',
    projects_root: null,
    config_dir: null,
    error: 'invalid TOML at line 3',
  });
  assert.equal(v.state, 'error');
  assert.equal(v.error, 'invalid TOML at line 3');
  assert.equal(v.installFile, '/h/.trusty-tools/install.toml');
  assert.deepEqual(v.rows, []);
  assert.equal(v.showEffectiveRoot, false);
});

test('a null path is omitted, and an older daemon without the field is absent', () => {
  const v = installPathsView({
    install_file: 'f',
    projects_root: null,
    config_dir: { path: '/c', source: 'env' },
    error: null,
  });
  assert.deepEqual(v.rows.map((r) => r.key), ['config_dir']);
  assert.equal(installPathsView(undefined).state, 'absent');
  assert.equal(installPathsView(undefined).showEffectiveRoot, true);
});

test('each documented source has a label; an unknown one shows verbatim', () => {
  assert.equal(sourceLabel('install-file'), 'install file');
  assert.equal(sourceLabel('config-template'), 'config template');
  assert.equal(sourceLabel('flag'), 'flag');
  assert.equal(sourceLabel('future-source'), 'future-source');
});
