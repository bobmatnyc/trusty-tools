Fixed

- `tm pr open` no longer rejects a PR body that ends with the attribution footer, a blank line, and the `https://claude.ai/code/session_…` link — the exact shape Claude Code's provisioned attribution tells a session to write. The footer may now be the last non-blank line or the last one above a single trailing session link, in either the bare or `Claude-Session:`-labelled form; a session link before the footer still passes and a body with no footer at all is still refused (#7297).
