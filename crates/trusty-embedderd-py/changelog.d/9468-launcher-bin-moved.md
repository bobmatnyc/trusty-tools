Breaking
- trusty-embedderd-py 0.2.0 is library-only: it no longer builds or installs the `trusty-embedderd-py` launcher binary. trusty-search now ships that binary, so `cargo install trusty-search` installs it next to `trusty-search` (#9468).
- If you installed the launcher on its own (`cargo install trusty-embedderd-py`), uninstall it with `cargo uninstall trusty-embedderd-py` before you upgrade trusty-search; cargo refuses to install a binary that another package owns. `tctl` removes the standalone install for you from the release that ships this change (#9468).
- New `trusty_embedderd_py::run()` holds the launcher logic the old `main` had; its CLI, arguments and exit codes are unchanged (#9468).
