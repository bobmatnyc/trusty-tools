Added

- A native macOS screen saver, `TrustyConsole.saver`, that displays the console
  dashboard: one `ScreenSaverView` hosting a `WKWebView` on
  `http://127.0.0.1:7788/ui/screensaver`. When the console is unreachable it
  paints a native Foundry-dark fallback and retries every 15s; the System
  Settings thumbnail renders the wordmark rather than spinning up a web view; and
  it reloads hourly for long-run memory hygiene. Port and route are overridable
  via `defaults -currentHost write com.trusty.console.saver ConsolePort <port>`.
  Source in `crates/trusty-console/macos/saver/`, built and installed by
  `scripts/build-console-saver.sh` and `scripts/install-console-saver.sh` —
  ad-hoc signed by default, Developer ID with `CODESIGN_IDENTITY` set. macOS-only
  (#6520).
