Changed
- `tm-session-pause` and the `tm-capabilities` workflows reference say the session-ref push now happens only on a GitHub-confirmed private `origin` (#TBD-pause-ref). A public, non-GitHub or unconfirmed `origin` gets no push and a `ref_error`; the agent reports it and never pushes the ref by hand.
