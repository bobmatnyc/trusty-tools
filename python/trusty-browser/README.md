# Trusty Browser

Trusty Browser routes a web-QA workflow to one enrolled Google Chrome profile, one exact page URL and one signed-in site account. The Python service listens on private Unix domain sockets. Chrome starts the bundled native messaging hosts over stdio. A claimed page shows its workflow in a banner and the profile's extension badge.

## Install on macOS

Use Python 3.12 or later and Google Chrome Stable in its standard application location. Run these commands from the repository root.

```sh
uv tool install ./python/trusty-browser
trusty-browser install-service
trusty-browser extension-path
```

The second command registers Chrome's two native hosts and starts the service in the current user's launchd session. The third prints the stable unpacked extension directory. Open `chrome://extensions` in each participating profile, enable Developer mode, choose **Load unpacked**, and select that directory. Use **Reload** there after updating the package and running `trusty-browser extension-path` again.

To run in the foreground, use `trusty-browser install-host` and `trusty-browser serve`. To inspect the launchd configuration, use `trusty-browser launchd-plist`. The service defaults to `~/.trusty/browser`. Use `--state /absolute/path` before a subcommand to select another private state directory. Keep the path short enough for macOS Unix sockets.

## Enroll and use a workflow

Open the supported, signed-in site. Click **Trusty Browser** in the Chrome toolbar. On HTTPS, choose **Enable access to this site**. Review the Chrome profile, site URL and site account shown in the popup. Enter a lowercase workflow name and choose **Enroll this workflow**.

If the workflow already belongs to another profile, the popup shows the current and proposed Chrome profiles, URLs and site accounts. Choose **Confirm replacement** to transfer the workflow and revoke its old grant, or **Cancel** to keep its route.

Configure one stdio MCP server per agent workflow. This example grants access to the workflow named `travel-qa`.

```json
{
  "mcpServers": {
    "trusty-browser": {
      "command": "trusty-browser-mcp",
      "args": ["--workflow", "travel-qa"]
    }
  }
}
```

The MCP client offers `browser_claim`, `browser_snapshot`, `browser_click` and `browser_release`. An agent must hold a lease before using a tab. A browser restart invalidates outstanding leases. Reopen the enrolled URL in the signed-in profile before claiming again. Only one tab in that profile may match the exact URL. If a click loses its acknowledgment, inspect the site before retrying.

## Supported sites

The bundled adapter supports pages implementing this contract. Generic third-party pages require a site-specific adapter.

- The same-origin `identity` endpoint, resolved relative to the enrolled page URL, returns JSON containing an `account` string derived from the authenticated server session. It must reject signed-out requests. The extension fetches it with credentials and caching disabled and requires a match with the rendered `#account` text and enrolled account.
- The page exposes input `#field`, button `#apply` and counter `#count`. For a click, the extension fills the marker and sets `button.dataset.expectedAccount` immediately before activating the button.
- The page reads and clears that expected account, submits it with the mutation and checks it against the server session **atomically before committing**. The example fixture uses the `X-Expected-Account` request header.
- On successful commit, the page updates its counter and dispatches `fixture-applied` on `document`. It dispatches `fixture-refused` if the server refuses the mutation.

The extension requests HTTPS access for the selected origin only on a popup gesture. The local QA fixture runs on HTTP at `127.0.0.1`. All browser control traffic uses stdio or UDS.

## Remove

In the enrolled Chrome profile, click **Remove workflow** and **Confirm removal**. The service revokes the workflow ticket, browser key and active claims. An operator can also run `trusty-browser revoke travel-qa`. For a profile that will be enrolled again after operator-side revocation, complete removal from its popup to clear its saved enrollment.

Run `trusty-browser uninstall-service` to stop the launchd job and remove its native-host registrations. Chrome manages removal of the unpacked extension in each profile. Use `trusty-browser revoke` for each workflow you want to remove from the private state directory.

## Tests

```sh
cd python/trusty-browser
uv run --extra test playwright install chromium
uv run --extra test pytest -q
node --test tests/test*.mjs
```

## Local trust boundary

The installed native hosts and Python interpreter are trusted code. Chrome and the broker authenticate each other during pairing. Subsequent command frames rely on the local pipes and Unix socket connecting those processes. The service wraps browser pairing keys and locally issued workflow tickets using an AES-GCM key in the macOS login Keychain. Copying the SQLite database and grant files alone does not reveal those credentials. An application that can execute code inside the trusted interpreter or access the same unlocked Keychain item can still act as the local user.

The [two-profile Chrome restart receipt](tests/evidence/two-profile-restart.json) records the workflow account checks, independent site journal, lease and control-indicator behavior, socket observations and cleanup for the pre-transfer build. The [installed-wheel transfer receipt](tests/evidence/workflow-transfer.json) covers cancel, confirm, recovery, revocation and the [live toolbar badge](tests/evidence/unclaimed-tab-toolbar.png).
