Added
- `tm install` and `tm fleet init` install the `trusty-browser-mcp` and `trusty-computer-mcp` binaries with `cargo install --locked` when they are missing, then register them at user scope. An existing entry is kept and reported. `tm doctor` reports each server as installed, registered and command-path-exists (G0b).
