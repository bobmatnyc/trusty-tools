Fixed
- With a broken `install.toml`, an in-place relaunch of a project pinned to a gh account refuses with an error naming the file, instead of writing the session's gh shim under `$HOME/.trusty-mpm/gh-shim` (#11821).
- With a broken `install.toml`, the daemon spawns a pinned project's session with a gh token that authenticates as nobody and logs a warning naming the file, instead of writing the session's gh shim under `$HOME/.trusty-mpm/gh-shim` (#11821).
