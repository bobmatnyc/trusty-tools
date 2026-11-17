# trusty-browser

Trusty Browser routes a web-QA workflow to one enrolled Google Chrome profile,
one exact page URL and one signed-in site account. A local broker listens on
private Unix sockets in `~/.trusty/browser`; Chrome reaches it through a native
messaging host, and local agents reach it through an MCP server.

This crate is the Rust port of the Python `trusty-browser` 0.7.0 package. It
keeps Python's socket paths, wire frames, registry schema, audit lines, Keychain
item and launchd job, so an existing Python install switches over without
re-pairing and can roll back.

## Install

macOS with Google Chrome Stable in its standard location. Run this outside any
Cargo workspace so no local `[patch]` applies:

```sh
cargo install trusty-browser --locked
trusty-browser install-service
trusty-browser extension-path
```

`install-service` writes Chrome's two native-host manifests and launchers and
the `com.trusty.browser` launchd job, then starts the broker in your login
session. `extension-path` prints the unpacked extension directory. In each
Chrome profile, open `chrome://extensions`, enable Developer mode, choose
**Load unpacked** and select that directory. After an upgrade, run
`extension-path` again and click **Reload**.

On macOS, never `cp` the installed binary to another path. Always install with
`cargo install`; a copied binary is killed at its next start for an invalid
signature.

## The two binaries

| Binary | Role |
|---|---|
| `trusty-browser` | The broker and operator CLI: `serve`, `install-host`, `install-service`, `uninstall-service`, `launchd-plist`, `extension-path`, `grants`, `revoke-grant`, `revoke-grants`, `pairings`, `unpair`, `revoke`, `native-host`, `migrate-keychain` |
| `trusty-browser-mcp` | The stdio MCP server that agents run. It connects to the broker's `agent.sock` |

`--state /absolute/path` before a verb selects another state directory. Keep
the path short enough for a macOS Unix socket.

## Cut over from the Python package

The Python broker's Keychain item is pinned to the uv Python that created it,
so the Rust binary does not read it. The trusted Python exports the key on a
pipe and `migrate-keychain` takes it over. The key never appears in argv or a
file.

1. Back up `~/.trusty/browser`, `~/Library/LaunchAgents/com.trusty.browser.plist`
   and the two manifests in
   `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/`.
2. `cargo install trusty-browser --locked`.
3. Stop the Python broker: `trusty-browser uninstall-service` from the Python
   install, or `launchctl bootout gui/$UID/com.trusty.browser`.
4. Hand the key over:

   ```sh
   ~/.local/share/uv/tools/trusty-browser/bin/python -c 'import os; from keyring.backends.macOS import Keyring; print(Keyring().get_password("com.trusty.browser.local-credentials", os.path.realpath(os.path.expanduser("~/.trusty/browser"))))' \
     | ~/.cargo/bin/trusty-browser migrate-keychain --from-stdin
   ```

   It unseals every pairing with the key before it replaces the Keychain item,
   and refuses a wrong key without changing anything.
5. `~/.cargo/bin/trusty-browser install-service`. It accepts the files Python
   wrote, replaces them, and restarts the job on the Rust binary.
6. Run `trusty-browser pairings` and confirm the same profiles are listed. Point
   your MCP client at `~/.cargo/bin/trusty-browser-mcp`.

Rollback: `trusty-browser uninstall-service`, restore the backups, and
bootstrap the restored plist. The two brokers never run together: both take
`service.lock`.

## Pending: Developer ID signing

`cargo install` produces an ad-hoc signed binary, and its code identity
changes on every reinstall. The Keychain item's access list is pinned to that
identity, so a reinstall can bring back a Keychain prompt, and a prompt under
launchd hangs the broker. A release signed with a stable Developer ID identity
is pending approval. Until it ships, run the binary once in your GUI session
after each reinstall and answer **Always Allow**, before relying on the
launchd job.

## License

MIT.
