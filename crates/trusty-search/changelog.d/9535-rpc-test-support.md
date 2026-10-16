Changed
- The socket parity tests share one `http_body`, `http_refusal` and `params` in `service::rpc::test_support`, and the `search.logs.tail` parity tests decode `n` with the socket's own `LogsTail::into_report_params` instead of an HTTP-only decode (#9535).
