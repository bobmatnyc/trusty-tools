Fixed

- The PM guard's script-body secret-read rule no longer probes its own process for a `/dev/fd/N` or `/proc/self/fd/N` script word. Such a word names the script shell's descriptor, so it now always refuses as computed at run time. Before, a guard that had fd N open read that descriptor as the script, and the refusal carried a reason that depended on what the guard happened to have open (#10748).
- The strict top-level path of the same rule now refuses an fd-pipe script word as computed at run time too, wherever the guard used to read the pipe: named once or more, run by its path, after a missing script, or behind a wrapper prefix the guard cannot resolve. Before, `bash /dev/fd/0 < x.sh ; : /dev/fd/0` allowed (#10748).
