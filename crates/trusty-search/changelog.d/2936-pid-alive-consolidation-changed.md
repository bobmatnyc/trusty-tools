Changed

- `service::daemon::pid_alive` is now `pub` and is the crate's single implementation. `commands::stop` carried a byte-identical private copy; the new staging reaper would have made a third. `pub` rather than `pub(crate)` because `commands::stop` lives in the binary crate and reaches this through the library's public `service` module. Additive only — no existing signature changed
