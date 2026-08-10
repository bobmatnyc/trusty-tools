Fixed

- The project picker scans the configured workspace root instead of a hardcoded `~/trusty-mpm-projects`, so retargeting the root via `TRUSTY_MPM_WORKSPACE_ROOT` or `workspace_root_template` no longer makes every real checkout look missing (#5203).
