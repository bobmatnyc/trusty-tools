Fixed
- Two `--lib` tests no longer fail intermittently in a full parallel run. The log-drain git-origin test runs its fixture git with no ambient git config, and the savings re-derivation test reads an injected roster instead of the host's `$CLAUDE_CONFIG_DIR`/`$HOME` roster. Production behavior is unchanged (#11731).
