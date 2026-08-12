Changed
- `trusty-audit tools` now reports the verified version beside each binary, and marks a binary this client did not place as `UNVERIFIED` rather than showing it as installed — a version it did not verify is one it cannot vouch for ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
- `Session::execute` is `async`. Installing downloads, and blocking on a runtime inside a synchronous `execute` would work for the CLI and then panic inside the Tauri shell, which calls from an async context ([#5495](https://github.com/bobmatnyc/trusty-tools/issues/5495))
