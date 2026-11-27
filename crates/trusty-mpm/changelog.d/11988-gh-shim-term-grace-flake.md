Fixed

- The `a_gh_that_ignores_term_is_killed_within_the_grace` test no longer KILLs a shim that is about to exit 143 under full-suite load: it now waits up to 15 s for the shim to exit after gh is reaped, while the 7 s bound on gh itself is unchanged (#11988).
