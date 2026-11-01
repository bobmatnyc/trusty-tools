Fixed
- The sandbox settings read-back refuses a project-tier `sandbox` key that tm's own policy does not write, such as `allowMachLookup` or `network.httpProxyPort`, and names the key. The merge keeps such keys, so before this verify passed them unread (#10102).
- The sandbox settings read-back refuses a `permissions.additionalDirectories` entry outside the workspace and the role's `allowWrite` roots, after resolving symlinks, the same way it checks `Edit(...)` allow rules (#10102).
- The sandbox settings read-back refuses `null` for `sandbox.excludedCommands` and for each sandbox-widening key, because Claude Code may skip a settings file its schema rejects (#10102).
