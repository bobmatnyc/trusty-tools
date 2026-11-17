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
   wrote, replaces them, and restarts the job on the Rust binary. First it
   signs `~/.cargo/bin/trusty-browser` in place with your Developer ID
   (identifier `com.trusty.browser`, team 4JH68XUHC5) and verifies it; if
   signing or verification fails it writes nothing. The identity comes from
   `--sign-identity`, then `TRUSTY_BROWSER_SIGN_IDENTITY`, then the one
   matching identity in `security find-identity -v -p codesigning`. The first
   signing can show a Keychain dialog asking to let `codesign` use the
   private key; answer it at the desk. Run `install-service` again after each
   `cargo install` upgrade so the new binary is signed.
6. Run `trusty-browser pairings` and confirm the same profiles are listed. Point
   your MCP client at `~/.cargo/bin/trusty-browser-mcp`.

Rollback: `trusty-browser uninstall-service`, restore the backups, and
bootstrap the restored plist. The two brokers never run together: both take
`service.lock`.

## Developer ID signing

`cargo install` produces an ad-hoc signed binary whose code identity changes
on every reinstall. The Keychain item's access list trusts the signed
identity, so `install-service` and `install-host` sign the binary they
register (see cutover step 5). A Developer ID signed binary keeps that
identity across rebuilds, so upgrades read the Keychain without a prompt.

`--no-sign` skips signing and prints a warning. Without signing every upgrade
brings back a Keychain prompt, and a prompt under launchd hangs the broker:
run the binary once in your GUI session after each reinstall and answer
**Always Allow** before relying on the launchd job.

`trusty-browser-mcp` is not signed by these verbs: neither launchd nor the
extension runs it, your MCP client does.

## License

MIT.
