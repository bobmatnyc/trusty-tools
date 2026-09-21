Added
- A pane COMMAND line longer than 960 bytes is now refused rather than typed, so a launch builder that still composes an unbounded line fails loudly instead of being silently truncated by the tty's canonical-mode input buffer. The guard is on the command-line senders only; task injection into Claude Code's raw-mode TUI, where the limit does not apply, is untouched (#8233).
