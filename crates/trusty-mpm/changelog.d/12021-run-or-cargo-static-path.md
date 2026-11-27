Fixed

- pm-guard test now pins `release-gate.sh` alongside the `--staged` gate scripts in `the_repo_gate_scripts_allow_8879`, so the static `install_paths.py` path in `scripts/lib/run_or_cargo.sh` stays covered (Refs [#12021](https://github.com/bobmatnyc/trusty-tools/issues/12021))
