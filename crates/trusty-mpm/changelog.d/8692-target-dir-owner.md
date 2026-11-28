Added
- `tm build-lease` now owns the target directory of a lease outside the slot pool. A pinned private `CARGO_TARGET_DIR`, and cargo's own `<workspace>/target` when `CARGO_TARGET_DIR` is unset, are held by an exclusive lock (`.trusty-checkout.lock`) for the build's whole life. A second leased build into that directory, from the same checkout too, waits and then exits 75 naming the lock, so it cannot replace binaries a running test or program uses (#8692, #9223).
- Such a lease now frees compile capacity once cargo drops its build lock, the same as a slot lease, so a second build elsewhere is admitted while its tests run (#8692).
- A target-dir lock that cannot be opened or locked, or a `Cargo.toml` that cannot be read or parsed when naming cargo's own directory, refuses the build at once with exit 75 naming the file (#8692).
- `tm doctor` has a `raw_cargo` row. It warns when a cargo outside the lease is compiling in this project's shared `CARGO_TARGET_DIR`, which the lease cannot refuse (#8692).
- Limit: an older `tm` takes no lock in an unset target directory and shares a kept one with its own checkout, so the protection holds only between leases of this `tm` or later.
