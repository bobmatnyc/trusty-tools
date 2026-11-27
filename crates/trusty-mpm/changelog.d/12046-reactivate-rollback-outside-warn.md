Fixed

- An in-place reactivate whose client gave up before the answer now rolls the session record back off `Active` at every log level. The rollback ran inside a `tracing::warn!` argument, so with WARN disabled it never ran and the record stayed `Active` (#12046).
