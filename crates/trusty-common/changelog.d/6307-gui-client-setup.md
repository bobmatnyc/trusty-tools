Added

- `gui_mcp_client` renders the MCP registration a GUI client launched by
  launchd needs: the absolute path of the running binary, read from
  `current_exe` and canonicalized, plus a working directory that exists
  (#6307). A GUI app started by launchd inherits
  `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which holds neither `~/.cargo/bin` nor
  any shim directory, so an entry whose `command` is the bare name
  `trusty-memory` exits 127 before the server speaks a byte of MCP and the
  client reports only that no tools were found. `build_entry` rejects both
  failing shapes — a relative command and a working directory that does not
  exist — before anything is written or printed. `configure` writes the client's
  own config file through `claude_config::patch_mcp_server` when the client
  keeps one, and hands the values back to be printed when it does not; ChatGPT
  desktop is the latter, so nothing on disk is touched for it.
