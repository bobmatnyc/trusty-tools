Fixed
- On Linux, the build-lease census counts each `tm build-lease` once instead of once per thread, so two or three running leases no longer fill the unleased-build ceiling and refuse every other build. When the process table lists a lease's threads without the process itself, the census still counts that lease once (#10260).
