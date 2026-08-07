Added

- `Bm25Client::missing_docs` asks the daemon which of a set of doc ids it does
  not hold — the only call that establishes coverage. `Bm25RpcError` and
  `is_method_not_found` let a caller tell a daemon that predates a method from
  one that is unreachable; both fail closed but need different operator action.
