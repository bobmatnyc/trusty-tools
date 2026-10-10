Fixed
- The daemon's session list and lookups no longer hang while the runtime-exit reaper waits on a stalled tmux call: the reaper releases the session store during its pane reads and re-checks the record before it writes, and leaves a record that changed in the meantime untouched ([#9034](https://github.com/bobmatnyc/trusty-tools/issues/9034))
