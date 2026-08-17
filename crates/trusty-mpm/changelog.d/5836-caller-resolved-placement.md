Fixed

- **The daemon-unreachable fallback's session now runs in the worktree it provisioned, not in the shared base clone** — `tm launch`'s managed-checkout redirect fired on every caller, including the ones that had already resolved placement themselves, so the fallback's fresh worktree and branch were abandoned and two concurrent fallback sessions landed in one tree. A caller that resolved placement says so (`LaunchDir::CallerResolved`) and the redirect is skipped
