Changed

- A builder dispatch is denied when the daemon cannot be reached or does not answer usably, rather than allowed. This is deliberately the opposite of the #4480 shared-worktree guard's policy on the same failure: a false allow there reproduces pre-guard behaviour, while a false allow here overcommits the host, and a machine that goes down takes every session with it. The inversion reaches builder dispatches only — everything else is classified before any daemon call (#6892).
