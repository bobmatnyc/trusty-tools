Fixed

- On macOS, `tm install` and `tm fleet init` sign the installed trusty-browser and trusty-browser-mcp binaries with `tctl sign trusty-browser` and print one line per binary, so the browser no longer waits for `install-service` or `install-host` to be signed. A binary reads as signed only when tctl exits 0 (Refs #11587).
