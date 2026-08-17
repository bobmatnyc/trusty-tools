Added

- Launching on a terminal now walks the operator through registration instead of printing "Next: register the repositories and boards to audit (`trusty-audit add`)" and exiting. It asks for one target at a time, registers each through the same validation `trusty-audit add` runs, shows the running list as entries land, and on an empty line carries on into tool installation and — after asking — the sweep, all in the one invocation. A refused target is reported and the prompt returns; only the terminal itself failing ends the session.
- With no controlling terminal the launch is unchanged: it prints the status card and prompts for nothing, so scripts and CI keep the shape they had.
- Targets registered with `add` now advance the guided flow. It read only the companion `manifest.toml`, which `tga audit` writes after a sweep completes, so it kept telling an operator to register what they had just registered.
