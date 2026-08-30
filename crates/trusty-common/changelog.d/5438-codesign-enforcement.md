Fixed

- The launchd-label scan now enforces its codesign-identifier exemption instead
  of assuming it. `signed_install_scripts_name_codesign_identifiers_by_convention`
  fails when a `scripts/install-*-signed.sh` binds an identifier under a name
  `codesign_stripped` cannot see, naming the script, the variable, the label and
  the `*_IDENTIFIER=` expectation. The stray-label panic message now tells a
  developer to check the namespace first: deriving a CODESIGN hit from the
  registry invalidates the binary's designated requirement and re-triggers macOS
  TCC prompts (#2558) — the rename #5436 had to undo by hand
  ([#5438](https://github.com/bobmatnyc/trusty-tools/issues/5438)).
