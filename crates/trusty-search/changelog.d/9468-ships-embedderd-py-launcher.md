Added
- `cargo install trusty-search` now also installs the `trusty-embedderd-py` Python/MPS sidecar launcher next to `trusty-search`, where the daemon looks for it first. Before this, no install path produced the launcher, so the Apple Silicon python bootstrap always failed and the daemon stayed on ort (#9468).
- Upgrading from a machine that ran `cargo install trusty-embedderd-py`: run `cargo uninstall trusty-embedderd-py` first, or cargo refuses the install because another package owns that binary name. `tctl` does this for you from the same release (#9468).
