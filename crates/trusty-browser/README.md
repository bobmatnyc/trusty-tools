# trusty-browser

`trusty-browser` is a local broker that lets AI agents drive your own
signed-in Google Chrome profiles. A Chrome extension that you pair with each
profile connects to the broker through a native messaging host. The broker
listens on private Unix sockets in `~/.trusty/browser`. Local agents reach it
through a stdio MCP server, `trusty-browser-mcp`. You consent with one
**Connect** click per profile, the extension can reach only the sites you
allow in Chrome, and the broker writes an audit line for every grant and
refusal. The crate supports macOS and Google Chrome Stable.

It is the Rust port of the Python `trusty-browser` 0.7.0 package. It keeps
Python's socket paths, wire frames, registry schema, audit lines, Keychain item
and launchd job, so an existing Python install switches over without
re-pairing and can roll back.

## Install

macOS with Google Chrome Stable in its standard location. Run this outside any
Cargo workspace so no local `[patch]` applies:

```sh
cargo install trusty-browser --locked
trusty-browser install-host
trusty-browser install-service
trusty-browser extension-path
```

- `install-host` writes Chrome's two native-messaging manifests and launchers.
- `install-service` does the same, writes the `com.trusty.browser` launchd
  job, and starts the broker in your login session. It runs `install-host`
  steps itself, so the two verbs are safe to run in either order.
- `extension-path` prints the unpacked extension directory. In each Chrome
  profile, open `chrome://extensions`, enable Developer mode, choose
  **Load unpacked** and select that directory. After an upgrade, run
  `extension-path` again and click **Reload**.

Both verbs sign the installed binaries with your Developer ID so the Keychain
trusts them across upgrades; `--no-sign` skips that step. See
[Developer ID signing](#developer-id-signing).

On macOS, never `cp` the installed binary to another path. Always install with
`cargo install`; a copied binary is killed at its next start for an invalid
signature.

### The two binaries

| Binary | Role |
|---|---|
| `trusty-browser` | The broker and operator CLI: `serve`, `install-host`, `install-service`, `uninstall-service`, `launchd-plist`, `extension-path`, `grants`, `revoke-grant`, `revoke-grants`, `pairings`, `unpair`, `revoke`, `native-host`, `migrate-keychain` |
| `trusty-browser-mcp` | The stdio MCP server that agents run. It connects to the broker's `agent.sock` |

`--state /absolute/path` before a verb selects another state directory. Keep
the path short enough for a macOS Unix socket.

## Consent and grant model

**Pairing.** The extension in a Chrome profile pairs with the broker once. The
pairing stores the profile id, the profile's account email and a browser key.
The broker seals the key with AES-GCM and keeps the sealing key in the macOS
Keychain. Later connections to `service.sock` prove the key with an HMAC-SHA256
handshake over fresh nonces from both sides. A wrong proof is refused and
audited.

**Enrollment.** Pair, unpair, connect and disconnect arrive from the extension
popup on `enrollment.sock`. Before it reads a frame, the broker checks that the
peer is the same user, that the peer is the native host launched by Chrome
itself, that Chrome did not start it with unsafe flags, and that the host passes a live `codesign` check. Each failure is an `Unauthorized`
reply with a `reason` code and a line in the audit log.

**Grants.** A **Connect** click in the popup creates a standing grant for that
one profile. Any local agent running as you can then attach to it. The grant
ends when you disconnect, when you revoke it with `trusty-browser revoke-grant`
or `revoke-grants`, or when the browser stays away longer than a 45 s
reconnect grace. Grants live in memory only; a broker restart ends them.
`trusty-browser grants` lists them.

**Site reach.** The grant covers a profile, not a site. Which sites the agent
can read or act on follows Chrome's own host permission: the extension holds
`https://*/*` only as an optional permission that you grant in the popup. On a
tab without that access, page ops answer `PermissionNeeded`. Navigation accepts
`https` URLs and `http://127.0.0.1` only.

**Leases.** An agent must lease a tab (`browser_select`) before it acts on it.
One agent controls a tab at a time; a second claim is refused `TabClaimed`. A
lease expires 60 s after its last use. A page op also pins the document it
read, so a navigation in between refuses with `DocumentChanged` or
`StaleElement` instead of acting on the wrong page.

**Retries.** Every action takes an optional `request_id`. Repeating an id
answers from the session journal and does not run the action twice. If a
mutating op loses its reply, the broker answers `OutcomeUnknown` and never
guesses.

**Refusals.** Replies are `{"ok":false,"error":<name>,...}`. Page-op names
include `OutOfScope`, `TabNotFound`, `PermissionNeeded`, `DocumentChanged`,
`NavigationPending`, `StaleElement`, `NotEditable`, `NotSelectable`,
`NoSuchOption`, `Obscured`, `DebuggerBusy`, `NoHistory`, `NotDraggable`,
`InvalidUrl`, `InvalidRequest`, `BrowserDisconnected` and `OutcomeUnknown`.

**Audit log.** `~/.trusty/browser/audit.jsonl` is an append-only, owner-only
(0600) JSON-lines file. It rotates at 5 MiB and keeps two older files. A line
records the event, grant, profile, op, tab, outcome, origin, site, email
domain and refusal reason. It records the domain, never the full address. If
the broker cannot write a line, it refuses the action (`AuditUnavailable`)
instead of acting unlogged.

**Peer checks.** The four sockets (`service.sock`, `agent.sock`,
`enrollment.sock`, `operator.sock`) are mode 0600 in a 0700 directory. The
broker also reads the kernel's record of the connecting user
(`LOCAL_PEERCRED` on macOS) and refuses any other uid. The native host
accepts only the extension's origin,
`chrome-extension://jabfhcolknbbdajilgpficniedhcfahc/`. Ops that need the Chrome debugger
(a dialog answered during a click, for example) attach `chrome.debugger` only to an
accessible tab still on the leased document, and detach afterwards.

Uploads need `--upload-root DIR` on `trusty-browser-mcp`: at most 10 files and
512 KiB in total, all inside that directory. A symlink or outside path is
refused.

## MCP tools

`trusty-browser-mcp` speaks stdio MCP. Descriptions below come from the
schemas in `src/mcp_connect_tools.json` and `src/mcp_workflow_tools.json`.
Action tools accept an optional `request_id` for retries.

### Connect mode (default)

The mode you use with a profile connected in the popup. No code is needed.

| Tool | What it does |
|---|---|
| `browser_profiles` | Lists the Chrome profiles you connected with the popup's Connect button. |
| `browser_connect` | Attaches to a connected profile. Omit `profile` when one is connected; pass the id when several are. |
| `browser_disconnect` | Closes this agent's session. The profile stays connected until you disconnect it. |
| `browser_tabs` | Lists the tabs this grant may address, with page access, origin and title. |
| `browser_select` | Leases a tab by id for this session without focusing it. |
| `browser_switch` | Brings the leased tab to the front and focuses its window. |
| `page_read` | Reads the page as elements with refs. Options: `scope_ref`, `viewport_only`, `include_text`, `max_chars` (1000 to 250000). |
| `page_find` | Finds elements by `role`, `name` or `text`; each match names its nearest list, row, dialog, form, navigation or main. |
| `page_click` | Clicks an element ref with a trusted mouse click. `dialog` answers a confirm, prompt or beforeunload dialog. |
| `page_hover` | Moves the pointer onto an element so menus and tooltips open. |
| `page_drag` | Drags one element ref onto another. |
| `page_type` | Replaces the text of an editable element, one keystroke per character. |
| `page_scroll` | Scrolls the tab, then waits up to `wait_ms` for new content. |
| `page_press` | Presses one key, with optional modifiers, on an element or the focused element. |
| `page_select` | Chooses an option of a native `<select>` by value or label. |
| `page_navigate` | Navigates the tab to an `https` URL or `http://127.0.0.1`. |
| `page_back` | Goes back one history entry; `NoHistory` when there is none. |
| `page_forward` | Goes forward one history entry. |
| `page_reload` | Reloads the page. |
| `page_upload` | Sets local files under `--upload-root` on an `<input type=file>`. |
| `page_downloads` | Lists the newest downloads started from the tab's origin, with path and state. |
| `browser_open_tab` | Opens a background tab at an `https` URL and returns it already leased to this session. |
| `browser_close_tab` | Closes a tab this session leases, including a user tab leased with `browser_select`; any other tab answers `TabNotLeased`. |
| `page_screenshot` | Captures the viewport, or one element `ref`, as a PNG image with a `capture_id` and the viewport metrics. Viewport only; a `ref` scrolls its element into view. |
| `page_snapshot` | Reads every frame of the tab, cross-origin and out-of-process iframes included, as the browser's accessibility tree. Each node carries a frame ref `f<frame>:<node>` that every ref tool accepts. Also lists the tabs this tab opened. |
| `page_click_at` | Clicks with a trusted mouse click at an image pixel of a `capture_id`. No hit-test: it can land inside a cross-origin iframe of the leased tab. Dismisses confirm and prompt dialogs. |
| `page_type_at` | Clicks at an image pixel of a `capture_id`, then inserts up to 8192 characters into the focused element as trusted input. No hit-test, as `page_click_at`. |
| `browser_release` | Releases a lease taken with `browser_select`. |

### Workflow mode (`--workflow NAME` or `--credential FILE`)

A narrower mode for a QA workflow tied to one enrolled page and identity.

| Tool | What it does |
|---|---|
| `browser_claim` | Verifies the configured Chrome and site identities, then claims the workflow's tab. |
| `browser_snapshot` | Reads the claimed tab after checking both identities again. |
| `browser_click` | Submits the marker to the enrolled page. |
| `browser_release` | Releases the workflow's claim. |

**Screenshots and coordinates.** `page_screenshot` uses CDP
`Page.captureScreenshot` over the existing `debugger` permission and adds no
permission. A coordinate action takes pixels of that image and its
`capture_id`. The broker maps them to CSS pixels by the image-to-viewport
ratio, which is `devicePixelRatio` (page zoom included) times any downscale
taken to fit the 1 MiB frame. A capture older than 5 s, or one whose viewport
size, ratio, zoom, scroll or document changed, answers `StaleCapture`, and a
point outside the viewport answers `OutOfViewport`. The agent still never
writes a selector.

**Frames.** `page_read` and `page_find` read the top document and its
same-origin frames. `page_snapshot` reads every frame. It attaches the
debugger once, sends CDP `Target.setAutoAttach` (flatten) so that each
out-of-process iframe becomes a child session, and reads
`Accessibility.getFullAXTree` per frame. It adds no permission. The broker numbers the frames
depth-first and keeps their ids per lease. A ref `f1:41` is node 41 of frame 1
of the lease's latest snapshot. A ref from an older snapshot, from before
the top document changed, or naming a node that snapshot did not list,
answers `StaleElement`. A frame that navigated
answers `DocumentChanged`, and one that was removed answers `StaleElement`.
`page_click` sums each out-of-process frame's offset into viewport CSS pixels,
hit-tests the point in the frame and in every parent document (an element
covering the iframe answers `Obscured`), checks the frame's document again
before the press, and sends trusted input. The other ref tools run their usual page function
in an isolated world of that frame. `page_click_at` and `page_type_at` reach
any frame already, because Chrome routes the input by position. The child
sessions are detached, then the tab, before each tool returns. Frame actions
run under the grant of the leased tab's top origin: a grant does not name the
origins of the frames inside the tab. A snapshot stops at 150 frames, node
depth 40 and about 700 KB, and then answers `truncated: true`; the text is
capped too. Coverage:

| Case | Behaviour |
|---|---|
| Cross-origin or out-of-process iframe, nested too | Read and acted on. |
| `about:blank`, `srcdoc` and sandboxed frames | Read and acted on. A frame where Chrome refuses an isolated world answers `FrameScriptBlocked` for the page-function tools; `page_click` and the coordinate tools still work. |
| Canvas | Pixels only: the node is marked, and is read with `page_screenshot` and acted on with `page_click_at`. |
| Chrome's PDF viewer | Listed with `refused: PdfViewer`. `page_screenshot`, `page_click_at` and `page_type_at` reach it. |
| A frame Chrome gives no tree for | Listed with `refused: FrameUnreachable`. |
| Popups and new windows the tab opened | Listed in `popups` with tab and window ids; lease one with `browser_select`. A tab grant lists none. |
| DevTools or another debugger on the tab | `page_snapshot` answers `page_read` with `fallback: page_read`, `reason: DebuggerBusy`, `partial: true` and a `note` that cross-origin frames were not read; other debugger tools answer `DebuggerBusy`. |

Chrome shows its "being debugged" bar while a debugger tool runs, as for
`page_click` and `page_screenshot`.

The tool set has no console or network tool. See the comparison
below.

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
   signs `~/.cargo/bin/trusty-browser` and `~/.cargo/bin/trusty-browser-mcp`
   in place with your Developer ID
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

Both verbs also sign `trusty-browser-mcp`, found beside the canonical
`trusty-browser` (same directory, same `cargo install`), with the same
identity and the same identifier `com.trusty.browser`. A workflow-mode
`trusty-browser-mcp` unseals its ticket from the same Keychain item at
start-up, and an MCP client cannot answer a prompt. Both binaries must sign
and verify or nothing is registered. A missing `trusty-browser-mcp` is an
error unless `--no-sign` is given.

## Comparison

Compared on 2026-11-18. Each row cites the project's own documentation, with
access date 2026-11-18. This section states what the sources say; it does not
test the products. Where a source did not answer, the cell says so.

| | trusty-browser | Claude in Chrome | Playwright MCP | Browserbase / Stagehand | browser-use | Chrome DevTools MCP |
|---|---|---|---|---|---|---|
| Whose browser | Your own Chrome profiles, paired | Your own Chrome, shared login state | Own profile, isolated, or (extension) your tabs in Chrome or Edge | Cloud browsers; Stagehand can also launch local Chrome | Cloud by default; local Chrome, a copy of a profile, or your open Chrome | A Chrome it launches, or your running Chrome |
| Runs where | Local only | Browser local; control goes through Anthropic's bridge | Local | Hosted (Browserbase) or local (Stagehand) | Hosted or local | Local |
| Consent model | One Connect per profile, in-popup; peer checks; audit log | Per-site permissions in the extension; admin allowlists on Team and Enterprise | Extension mode reuses your logged-in tabs; its consent step is not stated in the docs read | Not a user-session model; an account API key | Local debug port has no authentication | Dialog on `--autoConnect`; open port gives any local app control |
| Isolation | Per-profile grant, per-tab lease, 60 s TTL | Tab group per session | Isolated mode uses an in-memory profile | Fresh profile per session; Contexts persist state | Temporary profile option | `--isolated` temporary profile |
| Tool surface | 27 page and session tools, screenshots | Console, DOM, GIF, screenshots, uploads | Snapshots plus opt-in caps (vision, network, storage, pdf) | `act`, `observe`, `extract` | Agent writes CDP code | Performance, network, console, screenshots, automation |
| Browsers | Chrome on macOS | Chrome; Edge and Chromium forks detected | Chrome, Firefox, WebKit, Edge | Not stated in the pages read | Chrome, Chromium | Chrome (others unsupported) |
| Cost and license | MIT, free | Needs Pro, Max, Ultra, Team or Enterprise plan | Apache-2.0, free | MIT SDK; Browserbase from $0 to $99 a month | MIT; cloud $0.06 per browser-hour | Apache-2.0, free |

### trusty-browser

It controls only the Chrome profiles you pair, on your machine, with no hosted
component. It adds a local audit log, per-profile consent that the owner can
revoke, and a peer-verified native host. It is weaker on breadth: macOS and
Chrome only, no console or network tools, and no model of its own.
The grant covers a profile, so a connected agent can address any tab on which
Chrome has given the extension site access.

### Claude in Chrome

It also acts in your signed-in Chrome and pauses on logins and CAPTCHAs
([docs](https://code.claude.com/docs/en/chrome), accessed 2026-11-18). It is
stronger on tooling: console and DOM reads, GIF recording, file uploads and
Edge and Chromium-fork detection. It needs a direct Anthropic plan (Pro, Max,
Ultra, Team or Enterprise), a `/login` sign-in and the Chrome extension, and is
off under zero data retention
([same page](https://code.claude.com/docs/en/chrome)). The connection goes
through `bridge.claudeusercontent.com`, so it is not local-only. Site
permissions come from the extension; Team and Enterprise admins can set
allowlists and blocklists
([support article](https://support.claude.com/en/articles/12012173-getting-started-with-claude-in-chrome),
accessed 2026-11-18).

### Playwright MCP

It works from accessibility snapshots, is Apache-2.0 and runs Chrome, Firefox,
WebKit and Edge, with persistent, isolated and extension modes
([repo](https://github.com/microsoft/playwright-mcp), accessed 2026-11-18). It
is stronger on browser coverage and opt-in capabilities (network mocking,
storage, vision, PDF, tracing). The project states that it is not a security
boundary. Extension mode reuses your logged-in tabs in Chrome or Edge
([docs](https://playwright.dev/mcp/configuration/browser-extension), accessed
2026-11-18). That page gave no consent or token detail, so this README does not
describe it; check the current docs before relying on it.

### Browserbase and Stagehand

Browserbase runs hosted headless browsers with isolated sessions, live view and
replays
([docs](https://docs.browserbase.com/introduction/what-is-browserbase),
accessed 2026-11-18). Contexts persist cookies and auth between sessions and are
encrypted at rest, though the docs do not give the method
([docs](https://docs.browserbase.com/features/contexts), accessed 2026-11-18).
Stagehand is an MIT SDK in TypeScript, Python and Go with `act`, `observe` and
`extract`, and can launch a local Chrome
([repo](https://github.com/browserbase/stagehand), accessed 2026-11-18). Pricing
is Free ($0, 1 browser hour), Developer ($20 a month, 100 hours), Startup
($99 a month, 500 hours) and a custom Scale plan
([pricing](https://www.browserbase.com/pricing), accessed 2026-11-18). It is
stronger at scale, parallelism and unattended work. It does not drive your own
signed-in profile. The `mcp-server-browserbase` repository is archived, with a
hosted server recommended
([repo](https://github.com/browserbase/mcp-server-browserbase), accessed
2026-11-18).

### browser-use

An MIT Python library where a model writes code against Chrome DevTools
Protocol. `Browser()` defaults to a cloud browser. `Browser('chrome')` attaches
to your open Chrome once remote debugging is on, and `profile='Work'` uses a
temporary copy of a profile. The README warns that local debugging ports have no
authentication, so other users on a shared machine could reach the browser. The
library is free; the page cites $0.06 per cloud browser-hour
([repo](https://github.com/browser-use/browser-use), accessed 2026-11-18). It is
stronger as a full agent that plans and recovers on its own; trusty-browser
leaves planning to the calling agent.

### Chrome DevTools MCP

A Google-maintained Apache-2.0 server for performance traces, network and
console debugging, screenshots and automation. It launches Chrome itself or
connects to a running one with `--autoConnect` (Chrome 144 or later), which asks
you to click **Allow**. The docs warn that an open debugging port gives any
application on the machine control of the browser. Usage statistics are on by
default and can be disabled
([repo](https://github.com/ChromeDevTools/chrome-devtools-mcp), accessed
2026-11-18;
[advanced usage](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/main/docs/advanced-usage.md),
accessed 2026-11-18). It is stronger for debugging and performance work.

### Not verified

- Claude in Chrome's per-site permission UI: the support article did not
  describe it for individual users; the CLI page says site permissions are
  inherited from the extension.
- Playwright MCP extension mode's tab-selection and token behavior: the official
  page did not state it, and search summaries disagree.
- Browserbase SOC 2, HIPAA and data-residency claims: not on the pages fetched.
- Browserbase's isolation guarantees between concurrent sessions that share a
  Context: the docs do not describe them.
- Cost of Claude in Chrome beyond plan eligibility: no price was read here.
- Product behavior on dates after 2026-11-18.

## License

MIT.
