Fixed

- On macOS, `tm install` and `tm fleet init` sign the trusty-browser and trusty-browser-mcp binaries they just installed with `tctl sign trusty-browser` and print one line per binary; a browser already in place is not signed again, so the browser no longer waits for `install-service` or `install-host` to be signed. A binary reads as signed only when tctl exits 0. A failed sign makes the command exit non-zero; no identity configured (tctl exit 3) is a warning only (Refs #11587).
