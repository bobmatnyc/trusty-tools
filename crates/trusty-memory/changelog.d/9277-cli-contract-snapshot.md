Added
- A CLI contract snapshot test pins the trusty-memory clap tree (subcommand paths, flag names and short aliases, positional arity, value types, binary names and the usage-error exit code) and fails on a rename, a removal or an optional argument turning required (ADR-0066 D4, #9277). Refresh deliberately with `UPDATE_CLI_CONTRACT=1`.
