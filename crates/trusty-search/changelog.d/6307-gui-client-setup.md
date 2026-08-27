Fixed

- `trusty-search setup --client chatgpt` prints the registration a GUI MCP
  client can actually spawn: this binary's absolute path, the `serve` argument
  vector, and a working directory that exists (#6307). Configured with the bare
  command `trusty-search`, ChatGPT desktop failed the spawn with exit 127 —
  launchd starts GUI apps with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which
  contains no directory any trusty-* binary installs into — and reported only
  that no tools were found. The path is read from `current_exe`, so it is right
  wherever the binary was installed rather than assuming `~/.cargo/bin`, and the
  working directory replaces the client form's `~/code` default, which does not
  exist on every machine and fails the spawn on its own. ChatGPT desktop keeps
  no local MCP config file that a tool may write, so the command prints the
  three values to paste and changes nothing on disk.
