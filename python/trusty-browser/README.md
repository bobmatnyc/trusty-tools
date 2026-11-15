# Trusty Browser

Trusty Browser routes a web-QA workflow to one enrolled Google Chrome profile, one exact page URL and one signed-in site account. The Python service listens on private Unix domain sockets. Chrome starts the bundled native messaging hosts over stdio. A rust-colored border marks the edges of the controlled tab. The robot toolbar icon shows an ON badge while any tab in that profile is under control.

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

Open the supported, signed-in site. Click **Trusty Browser** in the Chrome toolbar. Choose **Enroll this site's workflow**. Chrome may ask for access to the selected site. Trusty verifies the signed-in Chrome profile and site account, selects the workflow, and pairs the extension. Only the robot’s eyes follow the pointer unless reduced motion is enabled. After enrolment the status line reads "Extension active"; expand **Activation details** to see the route and workflow ID.

If the workflow already belongs to another profile, the popup shows the current and proposed Chrome profiles, URLs and site accounts. Choose **Confirm replacement** to transfer the workflow and revoke its old grant, or **Cancel** to keep its route.

Configure one stdio MCP server per agent workflow. Use the workflow ID from **Activation details** in place of `travel-qa` in this example.

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

One-use activation codes let an agent drive one profile or one tab without a workflow ticket. For code mode, configure the server with no `--workflow` or `--credential` argument:

```json
{
  "mcpServers": {
    "trusty-browser": {
      "command": "trusty-browser-mcp"
    }
  }
}
```

Any Chrome profile can mint codes: click **Pair this profile** in its popup to pair it (no workflow is needed); the status line then reads "Profile paired" with the profile's email. Then choose this tab (the default) or the whole profile. The first code request also asks for access to all `https:` pages; the extension drops that access when the profile's last code or grant ends. Declining it leaves page operations limited to the tab you clicked.

The server starts and lists its tools even when the service is not running. Save the code from the popup to a file only you can read (mode `0600` or `0400`, not a symlink) and give the agent its path. The agent calls `browser_connect` with `code_file` set to that path, so the code never enters its transcript; `code` with the value itself still works. The agent then calls `browser_tabs` to list the tabs it may address, `browser_select` to lease one, `browser_switch` to bring a leased tab to the front (whole-profile grants only; a tab grant answers `OutOfScope`), and `page_read`, `page_type`, `page_click` and `page_navigate` on that lease. `page_read` returns element refs that `page_click` and `page_type` take. `browser_release` frees the lease and `browser_disconnect` ends the grant. If the service is down, `browser_connect` answers a `ServiceUnavailable` tool error; call it again once the service is up. Tool shapes and error frames are in "MCP tools", and the broker frame contract in the rest of [docs/activation-codes.md](docs/activation-codes.md).

An operator can list and end codes and grants with `trusty-browser grants`, `trusty-browser revoke-grant g-…` and `trusty-browser revoke-grants`, list paired profiles with `trusty-browser pairings`, and remove a pairing with `trusty-browser unpair <profile-id>`. Each verb prints the service's JSON reply and exits 1 on a refusal.

## Supported sites

The bundled adapter supports pages implementing this contract. Generic third-party pages require a site-specific adapter.

- The same-origin `identity` endpoint, resolved relative to the enrolled page URL, returns JSON containing an `account` string derived from the authenticated server session. It must reject signed-out requests. The extension fetches it with credentials and caching disabled and requires a match with the rendered `#account` text and enrolled account.
- The page exposes input `#field`, button `#apply` and counter `#count`. For a click, the extension fills the marker and sets `button.dataset.expectedAccount` immediately before activating the button.
- The page reads and clears that expected account, submits it with the mutation and checks it against the server session **atomically before committing**. The example fixture uses the `X-Expected-Account` request header.
- On successful commit, the page updates its counter and dispatches `fixture-applied` on `document`. It dispatches `fixture-refused` if the server refuses the mutation.

The extension requests HTTPS access for the selected origin only on a popup gesture. The local QA fixture runs on HTTP at `127.0.0.1`. All browser control traffic uses stdio or UDS.

## Remove

In the enrolled Chrome profile, open **Activation details**, click **Remove workflow** and **Confirm removal**. The service revokes the workflow ticket, browser key and active claims. An operator can also run `trusty-browser revoke travel-qa`. For a profile that will be enrolled again after operator-side revocation, complete removal from its popup to clear its saved enrollment.

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

The [two-profile Chrome restart receipt](tests/evidence/two-profile-restart.json) records the workflow account checks, independent site journal, lease and control-indicator behavior, socket observations and cleanup for the earlier build with a banner. The [installed-wheel transfer receipt](tests/evidence/workflow-transfer.json) covers cancel, confirm, recovery, revocation and the [live toolbar badge](tests/evidence/unclaimed-tab-toolbar.png).
