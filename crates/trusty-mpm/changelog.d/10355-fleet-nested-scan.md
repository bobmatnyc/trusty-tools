Fixed
- `tm fleet init` no longer refuses to relaunch an Architect whose directory holds more than 256 subdirectories. The nested-repository check for the fleet directory now visits up to 16,384 directories instead of the shared 256. A directory too large to check within that budget, or one that holds another repository, is still refused (#10355).
