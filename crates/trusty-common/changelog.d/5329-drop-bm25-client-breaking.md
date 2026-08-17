Breaking

- The public `bm25_client` module and the `bm25-client` feature that gated it
  are removed (#5329). Callers lose `Bm25Client`, its `new` / `for_palace`
  constructors, the `index` / `search` / `delete` / `stats` / `missing_docs`
  operations, and the `socket_path_for_palace` resolver. It was the UDS
  JSON-RPC client for the `trusty-bm25-daemon` subprocess, whose sole consumer
  was trusty-memory; #5329 moved that index in-process, so there is no socket,
  no wire protocol, and no binary to locate. A dependent that enables
  `bm25-client` no longer builds: switch to the `bm25` feature, which is the
  index itself (`BM25Index`) and needs no client. There is no deprecation
  period and no drop-in replacement for the remote-access shape — an in-process
  index is the only lexical lane now. The `uds` module and its `supervisor`
  submodule are untouched: trusty-console and trusty-agents supervise
  trusty-review and trusty-analyze through them. This is why `trusty-common`
  moves to `0.35.0` under Cargo's 0.x rule, where a breaking change bumps the
  MINOR position.
