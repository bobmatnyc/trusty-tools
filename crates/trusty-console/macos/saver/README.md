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
| `LoadHarness.swift` | Bundle-load smoke test — resolves the principal class and asserts the page loads. |

The bundle is assembled by `scripts/build-console-saver.sh` and copied into place
by `scripts/install-console-saver.sh`, both at the repo root.

## Build

```bash
bash scripts/build-console-saver.sh
```

Produces `target/console-saver/TrustyConsole.saver` and a `ditto` zip beside it.
The build reads the crate version from `crates/trusty-console/Cargo.toml` and
injects it as `CFBundleShortVersionString`, so the bundle and the crate never
disagree. Every run rebuilds in place.

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
  process, the web view is hidden and the view paints a native fallback: the
  Foundry dark background (`#201612`) with monospace `TRUSTY CONSOLE · offline`.
  It retries the load every 15 s while animating.
- **Preview** — the System Settings thumbnail (`isPreview == true`) never
  constructs a web view; it paints the wordmark in Foundry accent (`#d97742`).
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
