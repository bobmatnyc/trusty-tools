Fixed

- The PM guard's script-body secret-read rule no longer probes its own process for a `/dev/fd/N` or `/proc/self/fd/N` script word. Such a word names the script shell's descriptor, so it now always refuses as computed at run time. Before, a guard that had fd N open read that descriptor as the script, and the refusal carried a reason that depended on what the guard happened to have open (#10748).
