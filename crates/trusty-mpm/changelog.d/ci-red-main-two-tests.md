Fixed

- The prompt-feedback `Stop` / `SubagentStop` hook groups (#7688) now carry the
  same stable installed binary path the lifecycle triad does. They resolved
  their own command through the statusline resolver, which ignored the caller's
  pinned exe and falls back to the bare literal `tm` — so on a host with no
  installed `tm` on `PATH` the groups were written with a command Claude Code's
  minimal `PATH` cannot launch, while the triad beside them refused to write at
  all for that same condition (#7244). A launch that cannot resolve a stable
  binary now writes no capture group instead of a dead one (#1914).
