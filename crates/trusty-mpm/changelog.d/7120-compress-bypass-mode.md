Fixed
- `tm hook --divert-check` now diverts `cat -n <path>`, the whole-file read
  bypass-permissions mode instructs the agent to use. Every single-dash flag
  used to count as a bound, which is true of `head -n 40` and false of `cat -n`,
  so a large source read escaped diversion in bypass mode while the identical
  `Read` payload was diverted. A `cat <flags> <source file>` command is also no
  longer wrapped in `| tm compress`: the flag displaced the path out of the
  derived tool name, so the read reached the file-read filter the #6986
  passthrough exists to keep source away from, and paid a compress process spawn
  to get its own bytes back.
