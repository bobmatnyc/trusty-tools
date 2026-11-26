/**
 * View model for the Config tab's read-only install paths (#11821 P5).
 *
 * Why: the daemon resolves the project home and config dir from
 * `install.toml`. With a broken file it reports `error` and null paths, and the
 * tab must show that error rather than a default path that looks resolved. The
 * decision lives here, as a pure function, because no test runner in this
 * package can mount a Svelte component.
 * What: turns the `install_paths` object of `GET /api/console/config/mpm` into
 * `{ state, error, installFile, rows, showEffectiveRoot }`. State `absent` is a
 * daemon that predates the field; `error` hides the rows and the effective
 * root; `ok` carries the rows that have a resolved path.
 * Test: `installPaths.test.js`.
 */

const SOURCE_LABELS = {
  flag: 'flag',
  env: 'env',
  'install-file': 'install file',
  'config-template': 'config template',
  default: 'default',
};

/** Badge text for a `source` value; an unknown value is shown verbatim. */
export function sourceLabel(source) {
  return SOURCE_LABELS[source] ?? String(source);
}

const ROWS = [
  ['projects_root', 'Project home'],
  ['config_dir', 'Config dir'],
];

export function installPathsView(installPaths) {
  if (!installPaths || typeof installPaths !== 'object') {
    return { state: 'absent', error: null, installFile: null, rows: [], showEffectiveRoot: true };
  }
  const installFile = installPaths.install_file ?? null;
  if (installPaths.error) {
    return {
      state: 'error',
      error: installPaths.error,
      installFile,
      rows: [],
      showEffectiveRoot: false,
    };
  }
  const rows = ROWS.filter(([key]) => installPaths[key]).map(([key, label]) => ({
    key,
    label,
    path: installPaths[key].path,
    source: installPaths[key].source,
    sourceLabel: sourceLabel(installPaths[key].source),
  }));
  return { state: 'ok', error: null, installFile, rows, showEffectiveRoot: true };
}
