Fixed

- `tm sessions list` (and its deprecated `tm session` alias) now prints each row's real session id instead of the `--------` placeholder — serde writes the `SessionId` newtype transparently, so the daemon's wire value is a bare uuid string and the formatter only read the `{"0": …}` tuple shape; the resulting `-------- Starting <path>` row read as a session-start progress line rather than a list row (refs [#7805](https://github.com/bobmatnyc/trusty-tools/issues/7805))
