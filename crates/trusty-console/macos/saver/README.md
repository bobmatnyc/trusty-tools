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
| `PaintHarness.swift` | Paint regression harness — reads the rendered bitmap in the offline, slow-daemon and preview states (#6838). |

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

## Install

```bash
bash scripts/install-console-saver.sh                    # build, then install
bash scripts/install-console-saver.sh --from <bundle>    # install a prebuilt bundle
bash scripts/install-console-saver.sh --dry-run          # print every step, touch nothing
bash scripts/install-console-saver.sh --uninstall
```

It installs to `~/Library/Screen Savers/TrustyConsole.saver` with `cp -R`.

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
swiftc -swift-version 5 -o target/console-saver/harness/paintharness \
  crates/trusty-console/macos/saver/PaintHarness.swift

for mode in offline slow preview; do
  ./target/console-saver/harness/paintharness "$mode" \
    target/console-saver/TrustyConsole.saver || echo "FAILED: $mode"
done
```

| Mode | Endpoint | Asserts |
|---|---|---|
| `offline` | a closed port | within 1 s of `startAnimation()`, ≥98% of pixels are non-black and ≥2% carry drawn content (the text wordmark alone reaches ~0.4%) |
| `slow` | a listener that accepts and never answers | the same, plus ≥3 connection attempts in 34 s — i.e. the load timed out and retried instead of hanging on `URLRequest`'s 60 s default |
| `preview` | none (`isPreview: true`) | the bundled asset draws, and no `WKWebView` is built for a tile |

Exit 0 passes; 9 is an assertion failure (every failed assertion is printed);
2–6 and 8 are setup failures (bundle, principal class, endpoint, bitmap). Like
`LoadHarness` it runs **unsandboxed**, so it proves the drawing, not the
sandboxed host.

The `slow` mode is the regression guard for #6838: against an unfixed bundle it
reports one connection attempt, because nothing bounded the load.

### Manual verification (still owed)

The in-host run cannot be scripted. One operator step remains:

> System Settings → Screen Saver → select **TrustyConsole** → Preview.

Watch it decide, live:

```bash
log stream --predicate 'subsystem == "com.trusty.console.saver"'
```

Expect `init`, `loading`, then `didFinish`. An `offline` line instead means the
sandboxed `legacyScreenSaver.appex` host could not reach 127.0.0.1 — the one
thing the spike could not settle from outside the host.

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
  every 5 s for the first 3 minutes of an outage, then every 30 s, and switches
  to the live page the moment one succeeds — no saver restart.
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
- **Multi-display** — the framework instantiates one view per screen, so each
  display gets its own web view and timers. No coordination is attempted.

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
