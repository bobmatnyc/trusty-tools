Fixed
- The System Settings screen-saver gallery showed the generic placeholder tile
  instead of the console preview. The gallery reads
  `Contents/Resources/thumbnail.png` and `thumbnail@2x.png` by name and never
  instantiates the saver view, so the `isPreview: true` draw path added earlier
  could only ever reach the in-pane Preview.
  `scripts/build-console-saver.sh` now derives both files from
  `ConsolePreview.png` with `sips` — 90×58 and 180×116, centre-cropped to the
  tile aspect, matching the pair Apple's `Random.saver` ships
  ([#6839](https://github.com/bobmatnyc/trusty-tools/issues/6839)).
