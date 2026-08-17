Added

Launching on a terminal now walks the operator through registration instead of printing "Next: register the repositories and boards to audit (`trusty-audit add`)" and exiting. It asks for one target at a time, registers each through the same validation `trusty-audit add` runs, shows the running list as entries land, and on an empty line carries on into tool installation and — after asking — the sweep, all in the one invocation. A refused target is reported and the prompt returns; only the terminal itself failing ends the session.
