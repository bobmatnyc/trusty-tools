# TrustyConsole.saver — macOS screen saver

A native macOS `.saver` bundle that displays the trusty-console dashboard as a
screen saver. It is a thin wrapper: one `ScreenSaverView` hosting one
`WKWebView` pointed at `http://127.0.0.1:7788/ui/screensaver`. Every pixel of the
live view comes from the console SPA, so dashboard changes ship without
rebuilding the bundle.

Phase 4 of #6516 (issue #6520). macOS-only by construction — nothing here builds
or runs on Linux, and no CI job covers it (see "Not covered" below).

## Layout

| File | Role |
|---|---|
| `TrustyConsoleSaver.swift` | The `ScreenSaverView` subclass. The whole implementation. |
| `Info.plist` | Bundle plist template. `__CONSOLE_VERSION__` is replaced at build time. |
| `Resources/ConsolePreview.png` | Static render of the dashboard's services frame — the gallery tile and the offline fallback (#6839). Generated, committed, copied into `Contents/Resources/` at build time. |
| `LoadHarness.swift` | Bundle-load smoke test — resolves the principal class and asserts the page loads. |
| `PaintHarness.swift` | Paint regression harness — reads the rendered bitmap in the offline, slow-daemon and preview states (#6838), and tracks the web view across a late host resize (#6871). Runs at any frame size. |

The bundle is assembled by `scripts/build-console-saver.sh` and copied into place
by `scripts/install-console-saver.sh`, both at the repo root.

## Build

```bash
bash scripts/build-console-saver.sh
```

Produces `target/console-saver/TrustyConsole.saver` and a `ditto` zip beside it.
The build reads the crate version from `crates/trusty-console/Cargo.toml` and
injects it as `CFBundleShortVersionString`, so the bundle and the crate never
disagree. It also copies `Resources/ConsolePreview.png` into
`Contents/Resources/` and fails fast if that file is absent. Every run rebuilds
in place.

Two environment variables:

```bash
SAVER_ARCHS="arm64 x86_64" bash scripts/build-console-saver.sh   # universal via lipo
CODESIGN_IDENTITY="Developer ID Application: … (TEAMID)" bash scripts/build-console-saver.sh
```

`SAVER_ARCHS` defaults to `uname -m`. With `CODESIGN_IDENTITY` set the bundle is
signed Developer ID with `--options runtime --timestamp`; without it, ad-hoc
(`--sign -`). Either way the identifier is pinned to `com.trusty.console.saver`,
matching the workspace's stable-identifier convention
(`crates/trusty-installer/src/commands/macos_signing/mod.rs`,
`codesign_identifier()`). That table covers flat binaries on `PATH`; a directory
bundle is a different codesign shape and is signed by this script instead.

**Ad-hoc is enough to run it locally.** Developer ID and notarization matter only
for distribution past Gatekeeper — see "What the spike proved".

## Static preview asset

`Resources/ConsolePreview.png` is what the System Settings gallery tile and the
offline fallback draw. It is a real capture of `/ui/screensaver`'s services
frame, not a mock, so it goes stale when the dashboard changes. Regenerate it:

```bash
bash scripts/render-console-saver-preview.sh              # console must be running
bash scripts/build-console-saver.sh                       # copies it into the bundle
```

The script drives the Chromium that `website/`'s Playwright install already
caches (`pnpm install` inside `website/` if `node_modules` is absent), waits for
the 20 s rotation to reach the services frame with a populated roster, captures
1920×1080, and downscales with `sips` if the PNG lands over 500 KB. Overrides:

```bash
CONSOLE_URL=http://127.0.0.1:7790/ui/screensaver bash scripts/render-console-saver-preview.sh
PREVIEW_MAX_BYTES=300000 bash scripts/render-console-saver-preview.sh
```

Commit the regenerated PNG — the build copies it from the source tree, and a
bundle without it is rejected before the compile starts.

The gallery tile and the in-pane Preview are two separate mechanisms. System
Settings builds the tile by reading `Contents/Resources/thumbnail.png` and
`thumbnail@2x.png` by name; it never constructs the view, so the
`isPreview: true` draw path reaches only the Preview pane. The build derives
both thumbnails from the same `ConsolePreview.png` with `sips` — 90×58 and
180×116, centre-cropped to the tile aspect, matching the pair Apple's
`Random.saver` ships — so nothing extra is committed and the two can never
disagree (#6839).

## Install

```bash
bash scripts/install-console-saver.sh                    # build, then install
bash scripts/install-console-saver.sh --from <bundle>    # install a prebuilt bundle
bash scripts/install-console-saver.sh --dry-run          # print every step, touch nothing
bash scripts/install-console-saver.sh --uninstall
```

It installs to `~/Library/Screen Savers/TrustyConsole.saver` with `cp -R`, then
restarts System Settings, `legacyScreenSaver` and `WallpaperAgent` — all three
cache the saver bundle's display-name metadata (`CFBundleName`/
`CFBundleDisplayName`), so a renamed bundle keeps showing the old name in the
Screen Saver tile across a reinstall until they are restarted (#7128).
`--uninstall` does not restart them; see the #7128 comment in the script for
why.

**Why `cp` here and not `cargo install`.** CLAUDE.md bans `cp` for installing
release *binaries* on macOS: a `cp` over an on-`PATH` executable leaves a stale
kernel cdhash cache and the next exec is SIGKILL'd as an invalid signature. A
`.saver` is a directory bundle in a location that holds no `PATH` executables,
`cargo install` cannot produce one, and copying is Apple's own documented install
for the format. The rule does not reach this artifact.

## Verify

```bash
codesign --verify --deep --strict --verbose=2 ~/Library/Screen\ Savers/TrustyConsole.saver
```

### Smoke test (automated)

```bash
swiftc -swift-version 5 -o target/console-saver/harness/loadharness \
  crates/trusty-console/macos/saver/LoadHarness.swift

curl -s http://127.0.0.1:7788/health                    # console must be up
./target/console-saver/harness/loadharness \
  target/console-saver/TrustyConsole.saver \
  http://127.0.0.1:7788/ui                              # optional URL override
```

Exit 0 means the principal class resolved and the page finished loading. Exit
2–5 are bundle-load failures, 7 is "the page never loaded" (console down, or the
route 404s). The optional second argument writes `ConsolePort`/`ConsolePath` into
the module's own defaults domain and restores the previous values before exiting.

The harness runs **unsandboxed**, so a pass proves the bundle, the class name and
the URL — not that the sandboxed screen-saver host can reach the console.

### Paint harness (automated, #6838/#6839)

`LoadHarness` proves the happy path. `PaintHarness` proves the three states that
have no live page — the states the black screen was reported in. It reads the
view's own rendered bitmap through `cacheDisplay` and measures it, so it tests
the drawing code rather than navigation callbacks. **It needs no console
daemon**: each mode builds its own endpoint.

```bash
swiftc -O -swift-version 5 -o target/console-saver/harness/paintharness \
  crates/trusty-console/macos/saver/PaintHarness.swift

for mode in offline slow preview resize stop suspend suspend-cold recreate one-failure; do
  ./target/console-saver/harness/paintharness "$mode" \
    target/console-saver/TrustyConsole.saver || echo "FAILED: $mode"
done
```

`-O` matters: the pixel loop reads a million samples per frame and an
unoptimised build spends over a second in it.

| Mode | Endpoint | Asserts |
|---|---|---|
| `offline` | a closed port | ≥98% of pixels non-black and ≥2% carrying drawn content, both before `startAnimation()` and after |
| `slow` | a listener that accepts and never answers | the same, plus ≥3 connection attempts in 45 s — i.e. the load timed out and retried instead of hanging on `URLRequest`'s 60 s default |
| `preview` | none (`isPreview: true`) | the bundled asset draws, and no `WKWebView` is built for a tile |
| `resize` | the real console (7788, or `SAVER_HARNESS_PORT`) | after a late growth: `webView.frame == view.bounds`, the page's own `innerWidth`×`innerHeight` equals those bounds, and none of five edge samples is black (#6871) |
| `stop` | the same never-answering listener | `stopAnimation()` returns inside 500 ms and the listener sees **zero** further connections for 20 s — twice, once with a load in flight and once from inside a render tick (#6900) |
| `suspend` | a listener that **answers**, serving a page that reports `document.visibilityState` as `visible` for three reads then `hidden` | three re-entrant `startAnimation()` calls over the live page produce **zero** further requests; the page then goes 35 s reporting itself visible with **zero** requests, so a view that reloaded on every probe tick fails here; then the flip to `hidden` produces exactly one reload, and every frame captured while the web view is off screen carries ≥2% ink (#7112) |
| `suspend-cold` | the same listener, serving a page that is `hidden` from its **first** read | the same re-entrant assertion, then the view recovers inside 90 s with no visible-then-hidden history to reason from — the bounded forced-recovery deadline, measured at 69 s (#7112) |
| `recreate` | the never-answering listener again | after three consecutive failed loads the `WKWebView` **instance** is replaced and the replacement opens its own connection; the fallback keeps drawing across the swap (#7606) |
| `one-failure` | the same listener, watched only as far as the first failure and its retry | the instance is **not** replaced — the assertion that fails a view which spends a WebContent process on every brief console restart (#7606) |

### Stop path (#6900)

`loginwindow` stops the screen-saver host and waits for it before handing the
display to the unlock UI, so anything the view still does after the stop is time
the operator spends waiting instead of touching the sensor. The owner's report
was Touch ID not being offered; the unified log showed the stop request landing
at 18:35:26 and the saver still loading the console four minutes later.

`stopAnimation()` was never slow — it returns in about 0.2 ms on both the fixed
and the unfixed bundle. It simply did not stop the view. It set
`state = .offline`, which is the state `scheduleRetryTimer` reads as "keep
trying", left the navigation delegate attached, and then navigated to
`about:blank` over an in-flight console load. WebKit reported that cancellation,
`enterOffline` re-armed the retry timer the stop had just invalidated, and the
loop sustained itself from there.

So the mode measures traffic, not latency, and the budget is only a guard
against a future change putting a blocking wait into the stop path:

```
# unfixed bundle
PAINT: in-flight stop: stopAnimation() returned in 0.2ms
PAINT: in-flight stop: connection attempts after the stop: 2
PAINT: FAIL — in-flight stop: kept loading the console after the stop — 2 connection attempt(s), expected 0

# fixed bundle
PAINT: in-flight stop: connection attempts after the stop: 0
PAINT: PASS — stop
```

Between the two stops the mode calls `startAnimation()` again and requires a
fresh connection, so the terminal `.stopped` state cannot be sticky — a wake
that does not unlock stops and re-arms the saver, and that view has to load.

### Visibility recovery (#7112)

The owner's saver showed two rotation frames and then went black. Two behaviours
combined, and the mode covers both.

**The host re-arms a running view.** On macOS 26.6.2 the screen saver is hosted by
`WallpaperAgent`, which called `startAnimation()` every 20 s to 3.5 min on the
same live view with no `stopAnimation()` between. Every call reset the state and
reloaded, so the dashboard restarted from its first rotation frame each time —
and re-armed the hourly reload timer often enough that it could never fire.

**The OS suspends the page without killing it.** RunningBoard moved the
WebContent process to `running-suspended-NotVisible` and WebKit ran
`freezeAllLayerTrees` → `destroyRenderingResources` → `markAllLayersVolatile`,
discarding the backing store. The process stayed alive, so
`webViewWebContentProcessDidTerminate` — the view's only exit from `.live` — never
fired. `draw(_:)` went on deferring to a compositor with nothing left to
composite.

The view now asks the page every 10 s what `document.visibilityState` says. That
property is WebKit's own view of whether the page is visible, set by the same
activity-state change that freezes the layers, so it reports the cause rather
than the symptom. Anything but `visible`, or no answer inside 3 s, leaves `.live`
for `.suspended` — dimmed preview, no banner, because the console is reachable
and the reload is already in flight.

**Four grounds to recover on, and every path out is bounded.** An unhealthy
probe alone does not mean the page is broken — a saver whose window really is
behind something *should* have a hidden page. So `recoverVisibility` reloads on
any one of: the saver's own window is not occluded, so a hidden page contradicts
what is on screen; a visible-then-hidden transition was observed; the page
stopped answering at all; or the page has been unhealthy for 60 s. Only the
last-resort combination — an occluded window whose page has never reported
visible — waits, and that wait is what the 60 s deadline caps.

Deciding on the observed-transition flag alone was not enough, and the first cut
of this fix got it wrong. That flag resets on every `didFinish`, so a page
suspended inside its first 10 s probe interval answered `hidden` with no history,
only logged, and stayed live with the web view on screen — which the re-entrant
guard then read as healthy. The hourly reload was the only exit: up to an hour of
the exact symptom this issue is about.

`recoveryCooldown` caps the reload rate at one a minute, so none of the four
grounds can turn into a treadmill.

**And when reloading is not enough, the web view itself goes (#7606).** A reload
lands in the same WebContent process, so a page macOS has parked at
`running-suspended-NotVisible` comes back hidden and the recovery above reloads
it again. The owner's saver ran that loop for three days while the console
answered `/ui/screensaver` in under a millisecond throughout. After three
consecutive failures to get a live page on screen — a failed or timed-out load,
or a visibility recovery — the view tears down its `WKWebView` and builds a
fresh one, which is a fresh WebContent and network process, and loads into that.
The decision is made on the COUNT alone and never on what the page says about
its own visibility: a page reporting itself hidden inside a saver the host is
animating is not evidence of occlusion, and treating it as such is what let this
run for days. Only a probe answering `visible` clears the count —
`didFinish` does not, because every reload in that three-day loop finished. The
rebuild is capped at one a minute, widening to one per ten minutes once the
console has been down longer than `fastRetryWindow`, so a dead daemon overnight
does not become process churn. The same issue derived the retry delay from the
6 s load deadline rather than leaving it a flat 5 s underneath it: every retry
used to `load` over an attempt WebKit had not finished with, WebKit reported the
supersession as `NSURLErrorCancelled` (-999), and the view counted its own
cancellation as a fresh failure — 139 of them in two hours of the same log.

Every decision names its trigger and its ground at `.default`, so `log show`
separates them:

```
re-entrant startAnimation ignored — page already live
re-entrant startAnimation ignored — recovery already in flight
visibility lost, recovering — document.visibilityState=hidden (was visible)
visibility lost, recovering — document.visibilityState=hidden (unhealthy for 69s)
visibility lost, waiting — occluded window, page never reported visible (…)
window occlusion changed — visible=false
rebuilding the web view — 3 consecutive failed load(s): visibility recovery: …
rebuild held off — 3 consecutive failure(s), inside the 60s cooldown (…)
```

Against a bundle built from the pre-fix `main` (951d46b6b) both modes report
three failures each:

```
PAINT: FAIL — re-entrant startAnimation() reloaded the page — 3 extra request(s), expected 0
PAINT: FAIL — re-entrant startAnimation() took the live page off screen
PAINT: FAIL — the view never recovered: no reload in 35s after the page reported itself hidden
```

**What the modes do not prove.** Two things.

They drive the visibility flip from the page, because no unsandboxed harness can
make RunningBoard suspend a WebContent process.

And neither reaches the unoccluded-window ground. AppKit reports no `.visible`
for a window parked off every display, and setting `alphaValue` to 0 or 0.01 on
screen does not change that — the only way to produce an unoccluded window is to
put a real one in front of the operator, which the harness will not do. So
`suspend-cold` measures the 60 s deadline (69 s observed) rather than the
immediate ground the real full-screen saver would take. Whether the real
`WallpaperAgent`-hosted page reports `visible` at all before the NotVisible flip
is likewise settled only in host, and the log lines above say which happened.

### Frame size (#6871)

Every mode runs at the frame you give it, so the ultrawide geometry #6871 was
reported on is reachable:

```bash
./target/console-saver/harness/paintharness offline \
  target/console-saver/TrustyConsole.saver --frame 3440x1440
SAVER_HARNESS_FRAME=3440x1440 ./target/console-saver/harness/paintharness offline \
  target/console-saver/TrustyConsole.saver
```

The default is 1280×800, unchanged, so an invocation with no `--frame` still
measures what the ink table below was measured at. An explicit `--frame` wins
over `SAVER_HARNESS_FRAME`.

`resize` takes a second size: `--start WxH` is the frame the view is
CONSTRUCTED at before the host grows it to `--frame` (default 320×200, the rough
size of a System Settings preview). `--start 0x0` exercises the fully degenerate
case — a view built before the host knows which screen it is on. A 0×0 start has
no bitmap to read, so that run skips the two pre-resize captures and reports it.

`resize` is the one mode that wants a **live console**; the others build their
own endpoint and need none. Without one it still asserts the frame geometry and
prints `SKIP viewport check`.

Measured ink ratios, unfixed bundle → fixed:

| Mode | Unfixed @1280×800 | Fixed @1280×800 | Fixed @3440×1440 |
|---|---|---|---|
| `offline` | 0.0034 | 0.0417 | 0.0328 |
| `slow` | 0.0034 | 0.0417 | 0.0328 |
| `preview` | 0.0022 | 0.1152 | 0.0942 |

The ultrawide column is lower because the fallback asset is 16:9 and is drawn to
FIT: on a 21:9 frame it letterboxes, and about a quarter of the width is flat
background. The 2% bar is not per-frame and is still cleared.

Exit 0 passes; 9 is an assertion failure (every failed assertion is printed);
2–6 and 8 are setup failures (bundle, principal class, endpoint, bitmap). Like
`LoadHarness` it runs **unsandboxed**, so it proves the drawing, not the
sandboxed host.

The `slow` mode is the regression guard for #6838: against an unfixed bundle it
reports **one** connection attempt in 34 s, because nothing bounded the load.
The fixed bundle reports four.

**`resize` is not a regression guard for #6871, and cannot be made into one.**
Measured against a bundle built from the pre-fix `main` (37d6f11), it passes:
AppKit's autoresizing already keeps the web view at `bounds` through a
host-driven `setFrameSize`, from a 320×200 start and from 0×0 alike. What the
mode does guard is the invariant the fix makes explicit — that the web view owns
the whole view after a late resize — against a future change to the view
hierarchy that autoresizing would not survive. The ultrawide symptom itself
remains unreproduced outside the real host; see "Not covered".

The 1 s first-paint budget is a **machine-load** measurement, not a geometry one.
It fails on a busy host in every mode (2.7 s at 3440×1440 with a load average of
24 on 16 cores) because the clock spans WebKit's XPC bring-up. Report it; do not
raise it. `firstPaintDeadline` in `PaintHarness.swift` says why it is the
harness's weakest assertion.

### Manual verification (still owed)

The in-host run cannot be scripted. One operator step remains:

> System Settings → Wallpaper → "Screen Saver…" → select **Trusty Console** →
> Preview.
>
> <!-- #7128: the tile label is CFBundleDisplayName/CFBundleName, not the
> `TrustyConsole.saver` filename, which is unchanged. On this macOS build the
> Screen Saver pane lives under Wallpaper, not its own top-level item, and the
> x-apple.systempreferences:com.apple.ScreenSaver-Settings.extension URL lands
> on General instead. -->


Watch it decide, live:

```bash
log stream --predicate 'subsystem == "com.trusty.console.saver"'
```

Expect `init`, `loading`, then `didFinish`. An `offline` line instead means the
sandboxed `legacyScreenSaver.appex` host could not reach 127.0.0.1 — the one
thing the spike could not settle from outside the host.

**Reading it after the fact (#6871).** `log stream` only shows what happens while
you watch. The `init` and `startAnimation` lines are logged at `.default`, which
`log show` persists, so a report can carry the geometry the host actually handed
the view:

```bash
log show --last 30m --predicate 'subsystem == "com.trusty.console.saver"' --info
```

`init frame=…` is the frame the host passed to `init(frame:isPreview:)`;
`startAnimation bounds=…` is what the view owned by the time it loaded the page.
Those two numbers against the display's real resolution say whether a
fits-the-screen complaint is the host handing over the wrong frame or the page
laying out wrongly inside a correct one. Every other line stays at `.info` and is
memory-only — `log stream` sees them, `log show` will not.

## Port and route override

Both keys live in the module's per-host defaults domain:

```bash
defaults -currentHost write com.trusty.console.saver ConsolePort 7790
defaults -currentHost write com.trusty.console.saver ConsolePath /ui   # pre-#6519 consoles
defaults -currentHost delete com.trusty.console.saver                  # back to defaults
```

Defaults are port `7788` and path `/ui/screensaver`. An out-of-range port or a
path not starting with `/` is ignored rather than trusted. There is no
configuration sheet this phase (`hasConfigureSheet` is `false`).

## Behaviour

- **Live** — full-bounds `WKWebView` on the console route. `startAnimation()`
  loads it; `stopAnimation()` navigates to `about:blank`, which tears down the
  SPA and stops its metrics polling; the web view is released in `deinit`.
- **Offline** — on `didFailProvisionalNavigation` / `didFail` / a dead WebContent
  process / a load that has not finished in 6 s (a 5 s request timeout plus the
  view's own 1 s watchdog grace), the web view is hidden and the
  view paints `Resources/ConsolePreview.png` scaled to fit at 35% over the
  Foundry dark background (`#201612`), with a `TRUSTY CONSOLE · OFFLINE` banner
  over a scrim so a photograph of old numbers cannot read as live ones. Retries
  every 8 s for the first 3 minutes of an outage, then every 30 s, and switches
  to the live page the moment one succeeds — no saver restart. The 8 s is
  derived from the 6 s deadline, not chosen: a retry that lands inside it
  supersedes the attempt in flight and WebKit reports that as -999 (#7606).
- **Preview** — the System Settings thumbnail (`isPreview == true`) never
  constructs a web view; it paints the same asset at full opacity, with no
  banner.
- **Never black** — `animateOneFrame()` invalidates the view once per second
  while the live page is not on screen, so a first paint the full-screen host
  drops repaints within one tick rather than persisting until the next
  navigation callback (#6838).
- **No asset** — if `ConsolePreview.png` is missing from the bundle the view
  falls back to the monospace `TRUSTY CONSOLE · offline` wordmark. The build
  script refuses to produce such a bundle, so this is a defensive path.
- **Hourly reload** while animating, for long-run memory hygiene. The SPA polls
  its own data, so this is not a freshness mechanism.
- **Idempotent restart** — `startAnimation()` over a page that is already live
  and on screen, or over a recovery already in flight, reloads nothing.
  `WallpaperAgent` re-arms a running view every 20 s to 3.5 min with no
  `stopAnimation()` between (#7112).
- **Suspended** — while live, the view asks the page every 10 s whether WebKit
  still considers it visible. A page that stops reporting `visible`, or stops
  answering inside 3 s, is left for the dimmed preview (no banner — the console
  is reachable) and reloaded, on any of four grounds and after at most 60 s. This
  is the only exit from `.live` when the OS discards the page's layers without
  terminating its process (#7112).
- **Rebuilt** — after three consecutive failures to get a live page on screen
  (a failed or timed-out load, or a visibility recovery), the `WKWebView` is torn
  down and replaced, which is the only way to leave a frozen WebContent process
  behind. Decided on the count, never on what the page reports about its own
  visibility; cleared only by a probe answering `visible`; capped at one a
  minute, or one per ten minutes once the console has been down past the
  fast-retry window (#7606).
- **Multi-display** — the framework instantiates one view per screen, so each
  display gets its own web view and timers. No coordination is attempted.
- **Tracks the frame** — the web view is sized from `bounds` in
  `resizeSubviews(withOldSize:)` and again on the way into `startAnimation()`, so
  a host that constructs the view at a preview size or 0×0 and supplies the real
  screen afterwards cannot leave a mis-sized page on screen (#6871).
  `autoresizingMask` stays as well; it is a hint about size CHANGES, not a
  contract that the subview matches `bounds`.

Colours are copied from `docs/design/UI/design-system/tokens.css`
(`[data-theme='dark']`); the fallback is drawn natively and cannot pick up the
console's stylesheet.

## What the spike proved

A feasibility spike ran before this implementation. Four findings bind the code
and must not be "cleaned up":

1. **The principal class must be `public` and carry no `@objc(Name)` rename.** An
   explicit ObjC name changes the runtime class name, and the module-qualified
   `NSPrincipalClass` value then fails to resolve.
2. **`NSPrincipalClass` is `TrustyConsoleSaver.TrustyConsoleSaverView`** — the
   `<Module>.<Class>` form, which is what `NSClassFromString` resolves for a
   mangled Swift name.
3. **`swiftc -emit-library` output (an `MH_DYLIB`) loads fine as a bundle
   executable.** No Xcode project, no `.xcodeproj`, no bundle-loader dance.
4. **Ad-hoc signing loads inside the real host**, which carries
   `com.apple.security.cs.disable-library-validation`. Developer ID and
   notarization are a Gatekeeper-distribution concern, not a local-run one.

And one thing deliberately absent: **no `NSAppTransportSecurity` key.** ATS reads
the *host* application's plist, not a plug-in's, so the key is inert here — and
127.0.0.1 is exempt from ATS regardless.

Reference host for the spike: macOS 26.5.2 arm64, `LSMinimumSystemVersion 13.0`.

## Not covered

- **No CI job.** Every required check runs on Linux; a `.saver` needs a macOS
  runner. Adding one is a follow-up, not part of #6520.
- **No notarization step.** `scripts/build-console-saver.sh` stops at a signed,
  zipped bundle. Stapling is a distribution concern and nothing distributes this
  yet — see `docs/reference/release-workflow.md` for the workspace's Developer ID
  setup.
- **The ultrawide mis-fit is not reproduced outside the real host (#6871).** The
  `resize` mode grows the view the way a host that learns the screen late would,
  and the pre-fix bundle passes it — so whatever the owner saw on a 3440×1440
  display is not a plain `setFrameSize` the harness can drive. Two candidates the
  harness cannot reach: the sandboxed `WallpaperLegacyExtension` host sizing the
  view by a route that is not a subview resize at all, and the System Settings
  preview pane, which draws the 16:9 fallback asset letterboxed on a 21:9 pane
  and looks exactly like content that does not fit. The persisted `init frame=`
  and `startAnimation bounds=` lines above are what the next report should carry
  to tell those apart.
- **`cacheDisplay` cannot read a live page.** It runs the view's own `draw(_:)`,
  and a `WKWebView`'s remote layer is not part of that. The console's dark theme
  uses the same `#201612` the view fills with, so an edge sample of a live frame
  is background whether the page covers it or not. That is why `resize` asserts
  the web view's frame and the page's own viewport rather than trusting pixels.
