Fixed
- `url_userinfo::strip_url_secret` no longer reads an `@` in the path or query as the end of the userinfo when the authority is a plain `host:port` or an IPv6 `[...]` host: `https://host:8080/@scope/pkg` and `ssh://h:2222/o/r@x` are stored unchanged instead of as `https://scope/pkg` and `ssh://h@x`, so `tm register` keeps the clone URL intact (#9124). The log redactor keeps its over-read.
