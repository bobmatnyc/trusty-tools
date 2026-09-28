Fixed
- A build slot whose `tm build-lease` holder was killed with SIGKILL stays taken while the build it started is still running, and frees when that build exits. Cargo releases its `.cargo-lock` before it runs test binaries, so a `cargo test` run orphaned mid-run left its slot free and a second build could take it (#8261).
- The process at the recorded build pid must run the recorded program, matched by process name or executable. A pid the kernel reused for an unrelated process no longer holds the slot (#8736).
- A free slot whose leftover record is corrupt, or whose build pid, start time or process name cannot be read, is reported as broken instead of free. `tm build-lease` skips it and `tm doctor` fails on it, naming the slot file (#8736).
