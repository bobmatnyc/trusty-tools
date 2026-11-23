Fixed
- An `extends` child's own values in `[llm]`, `[compress]`, `[runner_config]`, `[session]`, `[plugins]`, `[rbac]` and `[workstreams]` are no longer discarded (#7901). These tables now merge per key: a key the child declares wins, and every key it omits is inherited from the base.
