Added

- `verification-before-completion` skill now carries the background-command wait protocol, the gate-chain pipe recipe (corrected: `pipefail` reports the LAST non-zero status, and `${PIPESTATUS[0]}` is unset under this harness's zsh, where `${pipestatus[1]}` is the per-stage code), and the stack-specific gate traps, all moved out of the resident BASE-AGENT body; `tm-workflow` gains a copy of the changelog-fragment placement and one-category rules, which stay resident in BASE-AGENT too (#8274).
