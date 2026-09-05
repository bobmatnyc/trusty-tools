Added
- `TrustyConsole.saver` bundles a static render of the dashboard's services
  frame. The System Settings gallery tile draws it instead of a text wordmark,
  and the pre-load/offline fallback draws it dimmed under a
  `TRUSTY CONSOLE · OFFLINE` banner. `scripts/render-console-saver-preview.sh`
  regenerates the PNG from the live `/ui/screensaver` page with the Chromium
  `website/`'s Playwright install already caches, so the asset can be refreshed
  whenever the dashboard changes
  ([#6839](https://github.com/bobmatnyc/trusty-tools/issues/6839)).
