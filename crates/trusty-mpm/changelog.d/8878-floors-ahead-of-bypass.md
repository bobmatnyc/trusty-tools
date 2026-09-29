Changed

- `TRUSTY_MPM_PM_UNRESTRICTED=1` and `TRUSTY_MPM_DISABLE_HOOKS` no longer lift
  the hard floor, for any session, the Architect's included: `rm -rf` of a
  filesystem root or home directory (or a delete whose target cannot be
  resolved), a read that would print a secret value, and the new
  upload, disk-tool and force-push rules now deny under both variables
  (#8878).
- The process-bound Architect's main thread is exempt from the rules that
  protect other sessions' work: the main-checkout write, commit and
  destructive-git rules, `rm -rf` of a worktree, and HEAD moves in a main
  checkout or linked worktree (#8878).
