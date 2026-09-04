Changed
- The `/ui/screensaver` route draws the same live 1 s bar graphs the home page
  does: one `machineStream.js` EventSource for the page seeds the window from
  the history snapshot and appends every second, the four host cards carry a
  graph on their bottom edge, and the newest streamed sample sets the card's
  headline number so it and the rightmost bar are the same second. Its service
  frame is now the alphabetical roster from `servicesList.js` — name, version,
  status, %CPU and a per-row CPU graph — rendered as an inert table, so nothing
  on the route is a button and nothing takes focus. The frame-0 service tally
  is that same list counted, so the two frames can no longer report different
  services. Idle entry, the fullscreen gesture and the poll backoff are
  unchanged ([#6643](https://github.com/bobmatnyc/trusty-tools/issues/6643)).
- `machineStatus.js` lost `serviceRows`, `serviceHealthTone` and `rollupTone`
  with the last view that rendered the metrics rollup as a table
  ([#6643](https://github.com/bobmatnyc/trusty-tools/issues/6643)).
