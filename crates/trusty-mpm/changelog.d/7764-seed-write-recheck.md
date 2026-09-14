Fixed

- The `CLAUDE.md` seed re-takes the workspace-parent guard immediately before it writes,
  so a child repository created between the first scan and the write can no longer be
  missed ([#7764](https://github.com/bobmatnyc/trusty-tools/issues/7764))
