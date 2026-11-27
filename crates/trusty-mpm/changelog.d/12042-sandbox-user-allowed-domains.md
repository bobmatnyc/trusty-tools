Fixed

- Sandboxed sessions can reach GitHub, crates.io and npm again. tm now writes `sandbox.network.allowedDomains` into its user-level settings next to `strictAllowlist`, because Claude Code ignores project-tier `allowedDomains` while `strictAllowlist` is on. Before this fix, every host was denied. The next launch repairs a user settings file that 1.14.14 left with `strictAllowlist` and no hosts, and keeps the operator's other keys. Verify now refuses a launch when the user tier has no hosts, an empty list, or a host outside tm's list (#12042).
