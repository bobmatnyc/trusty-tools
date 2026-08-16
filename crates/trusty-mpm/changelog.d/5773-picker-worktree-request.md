Added

- The interactive picker can ask for an isolated session: `n <name> --worktree` (the flag also leads, and works alone) sends the same `worktree` request `tm launch --worktree` sends, so a long refactor or a risky rebase no longer needs a different command. Without the flag nothing changes — the launch still lands in the project's main checkout.
- A launch that joins a checkout other live sessions are already standing in prints one line naming them, the ordinal, and the `--worktree` alternative. It states where the session lands; it does not warn, prompt, or refuse, and the first session in a project sees nothing.
