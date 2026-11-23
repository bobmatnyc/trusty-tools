Added
- `page_emulate` disables JavaScript, forces `prefers-color-scheme` light or dark, and sets the viewport size of a leased tab; a call with the defaults resets it. The emulation resets when the lease ends: release, expiry, session close, grant end, a dropped native port or a worker restart, and before a new lease's first probe; a reset the worker refuses is retried (#11371).
