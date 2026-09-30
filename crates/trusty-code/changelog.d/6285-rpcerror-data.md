Fixed

- **The socket transport no longer drops a JSON-RPC error's `data` (#6285).**
  Converting Trusty Code's `RpcError` onto `trusty_common::uds::server::RpcError`
  now carries `data` through when present, so a caller over the unix socket reads
  the same structured error detail it gets over STDIO or HTTP.
