Fixed
- The pm-guard refusal of an unleased heavy build now skips a build that follows a remote runner word (`$EVO`, `${EVO}`, `evo`, `ssh`), so the EVO gate form `$EVO cargo test -p <crate>` runs again; a local wrapped build such as `entr cargo test` is still refused (#11886).
