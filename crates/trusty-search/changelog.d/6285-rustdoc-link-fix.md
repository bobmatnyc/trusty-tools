Fixed

- Resolved 12 broken rustdoc intra-doc links in `service::rpc`'s module-level
  docs (`queries`, `writes`, `streams`, `chat`) — private helper names (`guarded`,
  `bulk_guarded`, `unguarded`) are now plain code spans rather than dead links,
  and public items (`register`, `IndexBody`, `SearchAppState`, `METHOD_CHAT`)
  resolve through fully-qualified reference-style links — unblocking the
  rustdoc intra-doc-link publish gate. No behavior change (#6285).
