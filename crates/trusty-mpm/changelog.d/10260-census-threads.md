Fixed
- On Linux, the build-lease census counts each `tm build-lease` once instead of once per thread, so two or three running leases no longer fill the unleased-build ceiling and refuse every other build (#10260).
