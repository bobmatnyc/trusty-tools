Added
- `[pm_guard] documents_repos` in `~/.trusty-mpm/config.toml` lists documents repositories whose main checkout may write and commit any path, source extensions included, so a repository that forbids worktrees can commit a utility script. Entries match the checkout root by canonical path. The list is trusted to the #8878 trust-anchor floor, with the #8879 residual (#7905).
