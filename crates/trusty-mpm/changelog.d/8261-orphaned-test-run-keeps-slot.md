Fixed
- A build slot whose `tm build-lease` holder was killed with SIGKILL stays taken while the build it started is still running, and frees when that build exits. Cargo releases its `.cargo-lock` before it runs test binaries, so a `cargo test` run orphaned mid-run left its slot free and a second build could take it (#8261).
- A free slot whose leftover record is corrupt, or whose build pid or start time cannot be read, is reported as broken instead of free. `tm build-lease` skips it and `tm doctor` fails on it, naming the slot file (#8736).
- A released build slot is free at once, even while the same process is spawning children. The slot lock was only released when the file closed, and a child in the middle of being spawned briefly held a copy of that file (#8736).
