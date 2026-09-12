Fixed

- The analyze dashboard renders in the faces its tokens ask for. `index.html`
  loaded Inter and JetBrains Mono while `styles/tokens.css` has always resolved
  `--trusty-font` / `--trusty-mono` to IBM Plex Sans / IBM Plex Mono, so neither
  requested face was ever used and the page fell back to the system stack. It
  now loads the Foundry three-face set that ui-search and ui-memory already
  load, which is also where the brand lockup's Chakra Petch wordmark comes from
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)).
