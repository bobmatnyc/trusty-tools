Changed

- Pinned `object_store` to 0.13 (from 0.14) for the `log-drain` feature. 0.14
  moved to `reqwest` 0.13, `md-5` 0.11 and a new `crc-fast`, each of which lands
  as a second copy of a crate the workspace already resolves — the workspace,
  `async-openai`, `teloxide-core`, `hf-hub` and `reqwest-eventsource` are all on
  `reqwest` 0.12, `lopdf` holds `md-5` 0.10, and `ring` holds `spin` 0.9. 0.13.2
  needs none of them and the drain uses no 0.14-only API, so this drops three
  duplicate crate versions and keeps a second reqwest/hyper/rustls stack out of
  every binary. Revisit when the workspace itself moves to `reqwest` 0.13
  (#6549).
