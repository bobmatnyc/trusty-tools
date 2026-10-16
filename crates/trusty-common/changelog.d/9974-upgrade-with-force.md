Added
- `update::perform_upgrade_with_force` and `update::perform_upgrade_captured_with_force` run `cargo install <crate> --locked --force` under the same #5777 ownership guard as `perform_upgrade` / `perform_upgrade_captured`; with `force = false` the command is unchanged. trusty-installer uses them to take the `trusty-embedderd-py` launcher over from a standalone install (#9974).
