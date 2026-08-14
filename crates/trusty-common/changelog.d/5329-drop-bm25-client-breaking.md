Breaking

- The public `bm25_client` module and its `bm25-client` feature are removed
  (#5329). Any dependent enabling `bm25-client` fails to build; switch to the
  `bm25` feature, which is the index itself and needs no client. This is why
  `trusty-common` moves to `0.34.0` under Cargo's 0.x rule, where a breaking
  change bumps the MINOR position.
