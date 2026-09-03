Changed
- The console home page has one services section, titled "Services": an
  alphabetical list carrying name, version, status, %CPU and a per-second CPU
  bar graph per row. Clicking a row opens that service's dashboard; a service
  with no dashboard renders inert and says so. The "Installed Services" card
  grid and the machine-status rollup table are both gone, along with
  `ServiceCard.svelte` and `cardActions.js`
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
