// TrustyConsoleSaver — the macOS screen-saver bundle for trusty-console (#6520,
// Phase 4 of epic #6516).
//
// Why: the console already renders a full machine-status dashboard at
//   `/ui/screensaver` (#6519). macOS has no way to run a web page as a screen
//   saver, so the route needs a native `.saver` bundle around it. This file is
//   that wrapper and nothing more — every pixel of the live view comes from the
//   console SPA, so dashboard changes ship without rebuilding the bundle.
// What: a `ScreenSaverView` subclass hosting one full-bounds `WKWebView` pointed
//   at `http://127.0.0.1:<port><path>`, with a native offline fallback, a 15 s
//   retry while the console is down, and an hourly reload for long-run memory
//   hygiene.
// Test: `LoadHarness.swift` in this directory resolves the principal class,
//   instantiates the view outside the screen-saver host and asserts `didFinish`
//   fires. The in-host run is manual — see README.md, "Manual verification".
//
// Two constraints below are load-tested spike findings, not preference:
//   * the class is `public` and carries NO `@objc(Name)` rename, so the runtime
//     name stays the mangled Swift one that `NSPrincipalClass` resolves from its
//     module-qualified `TrustyConsoleSaver.TrustyConsoleSaverView` form;
//   * no `NSAppTransportSecurity` key is shipped in Info.plist — ATS reads the
//     HOST's plist, not the plug-in's, and 127.0.0.1 is exempt from ATS anyway.

import AppKit
import Foundation
import ScreenSaver
import WebKit
import os.log

private let saverLog = OSLog(subsystem: "com.trusty.console.saver", category: "saver")

/// Why: the offline and preview states are drawn natively, so they cannot pick up
///   the console's stylesheet. Hardcoding the Foundry dark-theme values keeps the
///   fallback visually continuous with the live dashboard.
/// What: the four `[data-theme='dark']` tokens this view paints with, copied from
///   `docs/design/UI/design-system/tokens.css`.
/// Test: visual — the fallback matches the console's dark background.
private enum Foundry {
    /// `--trusty-content-bg: #201612`
    static let background = NSColor(srgbRed: 0x20 / 255.0, green: 0x16 / 255.0, blue: 0x12 / 255.0, alpha: 1)
    /// `--trusty-text-primary: #f0e7d8`
    static let textPrimary = NSColor(srgbRed: 0xf0 / 255.0, green: 0xe7 / 255.0, blue: 0xd8 / 255.0, alpha: 1)
    /// `--trusty-text-muted: #a58a6b`
    static let textMuted = NSColor(srgbRed: 0xa5 / 255.0, green: 0x8a / 255.0, blue: 0x6b / 255.0, alpha: 1)
    /// `--trusty-accent: #d97742`
    static let accent = NSColor(srgbRed: 0xd9 / 255.0, green: 0x77 / 255.0, blue: 0x42 / 255.0, alpha: 1)
}

/// Why: the console's port is site-local — an operator who moved it off 7788 must
///   be able to retarget the saver without a rebuild, and while #6519 is unmerged
///   the route path itself has to be steerable at `/ui`.
/// What: reads `ConsolePort` (integer) and `ConsolePath` (string) from the
///   per-host screen-saver defaults domain `com.trusty.console.saver`, falling
///   back to 7788 and `/ui/screensaver`. An out-of-range port or a path that does
///   not start with `/` is ignored rather than trusted.
/// Test: `LoadHarness.swift` writes both keys before instantiating the view and
///   restores them afterwards, so a harness run exercises this resolution.
struct SaverConfig {
    static let defaultsDomain = "com.trusty.console.saver"
    static let portKey = "ConsolePort"
    static let pathKey = "ConsolePath"
    static let defaultPort = 7788
    static let defaultPath = "/ui/screensaver"

    let port: Int
    let path: String

    var url: URL {
        // 127.0.0.1 rather than localhost: no DNS, and it is the address the
        // console binds by default (crates/trusty-console/src/bind.rs).
        URL(string: "http://127.0.0.1:\(port)\(path)") ?? URL(fileURLWithPath: "/")
    }

    static func current() -> SaverConfig {
        let defaults = ScreenSaverDefaults(forModuleWithName: defaultsDomain)
        let storedPort = defaults?.integer(forKey: portKey) ?? 0
        let port = (1...65535).contains(storedPort) ? storedPort : defaultPort

        let storedPath = defaults?.string(forKey: pathKey) ?? ""
        let path = storedPath.hasPrefix("/") ? storedPath : defaultPath

        return SaverConfig(port: port, path: path)
    }
}

/// Why: the principal class System Settings and `ScreenSaverEngine` instantiate.
/// What: hosts the console dashboard in a `WKWebView`, or paints a native
///   wordmark when the console is unreachable or macOS is only asking for the
///   System Settings thumbnail.
/// Test: `LoadHarness.swift`; in-host verification is manual (README.md).
public final class TrustyConsoleSaverView: ScreenSaverView, WKNavigationDelegate {

    /// What the view is currently showing. Only `.live` puts the web view on screen.
    private enum DisplayState {
        case live
        case offline
        case preview
    }

    /// Retry cadence while the console is unreachable.
    private static let retryInterval: TimeInterval = 15
    /// Full reload cadence while animating — long-run memory hygiene, not freshness
    /// (the SPA polls its own data).
    private static let reloadInterval: TimeInterval = 3600

    private var webView: WKWebView?
    private var retryTimer: Timer?
    private var reloadTimer: Timer?
    private var state: DisplayState
    private let config = SaverConfig.current()

    // The ScreenSaver framework instantiates ONE view per attached screen, so a
    // multi-display machine runs N independent views, each with its own web view
    // and timers. That is the framework default and needs no coordination here.
    public override init?(frame: NSRect, isPreview: Bool) {
        state = isPreview ? .preview : .offline
        super.init(frame: frame, isPreview: isPreview)

        animationTimeInterval = 1.0
        autoresizingMask = [.width, .height]

        os_log("init isPreview=%{public}@ url=%{public}@",
               log: saverLog, type: .info,
               String(describing: isPreview), config.url.absoluteString)

        // The System Settings thumbnail is a few hundred pixels of a dashboard
        // nobody can read, and spinning up a WebContent XPC child for it is pure
        // cost. Preview draws the wordmark natively instead.
        guard !isPreview else { return }

        let web = WKWebView(frame: bounds, configuration: WKWebViewConfiguration())
        web.autoresizingMask = [.width, .height]
        web.navigationDelegate = self
        web.setValue(false, forKey: "drawsBackground")
        web.isHidden = true
        addSubview(web)
        webView = web
    }

    @available(*, unavailable)
    public required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used by the screen-saver host")
    }

    deinit {
        retryTimer?.invalidate()
        reloadTimer?.invalidate()
        webView?.navigationDelegate = nil
        webView?.removeFromSuperview()
        webView = nil
    }

    // MARK: - ScreenSaverView

    public override func startAnimation() {
        super.startAnimation()
        guard state != .preview else { return }
        loadConsole()
        scheduleReloadTimer()
    }

    public override func stopAnimation() {
        super.stopAnimation()
        retryTimer?.invalidate()
        retryTimer = nil
        reloadTimer?.invalidate()
        reloadTimer = nil
        guard state != .preview else { return }
        // about:blank tears down the SPA, which stops its metrics polling. Without
        // this the console keeps being polled by an off-screen saver.
        if let blank = URL(string: "about:blank") {
            webView?.load(URLRequest(url: blank))
        }
        webView?.isHidden = true
        state = .offline
        os_log("stopAnimation — navigated away", log: saverLog, type: .info)
    }

    /// Why: the fallback has to render even when no web view exists (preview) or the
    ///   web view is hidden (offline), and a transparent screen saver is
    ///   indistinguishable from a crashed one.
    /// What: fills the Foundry dark background, then centres the wordmark unless the
    ///   live dashboard is on screen.
    public override func draw(_ rect: NSRect) {
        Foundry.background.setFill()
        rect.fill()
        guard state != .live else { return }
        drawWordmark()
    }

    public override var hasConfigureSheet: Bool { false }
    public override var configureSheet: NSWindow? { nil }

    // MARK: - Loading

    private func loadConsole() {
        guard let webView else { return }
        os_log("loading %{public}@", log: saverLog, type: .info, config.url.absoluteString)
        var request = URLRequest(url: config.url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        webView.load(request)
    }

    private func scheduleReloadTimer() {
        reloadTimer?.invalidate()
        reloadTimer = Timer.scheduledTimer(withTimeInterval: Self.reloadInterval, repeats: true) { [weak self] _ in
            guard let self, self.state == .live else { return }
            os_log("hourly reload", log: saverLog, type: .info)
            self.loadConsole()
        }
    }

    private func scheduleRetryTimer() {
        guard retryTimer == nil else { return }
        retryTimer = Timer.scheduledTimer(withTimeInterval: Self.retryInterval, repeats: true) { [weak self] _ in
            guard let self, self.state == .offline else { return }
            self.loadConsole()
        }
    }

    private func enterOffline(_ reason: String) {
        state = .offline
        webView?.isHidden = true
        needsDisplay = true
        os_log("offline — %{public}@", log: saverLog, type: .error, reason)
        scheduleRetryTimer()
    }

    // MARK: - Native fallback

    private func drawWordmark() {
        let size = max(14, bounds.height * 0.035)
        let font = NSFont.monospacedSystemFont(ofSize: size, weight: .medium)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center

        let text = NSMutableAttributedString(
            string: "TRUSTY CONSOLE",
            attributes: [
                .font: font,
                .foregroundColor: state == .preview ? Foundry.accent : Foundry.textPrimary,
                .kern: size * 0.18,
                .paragraphStyle: paragraph,
            ])
        if state == .offline {
            text.append(NSAttributedString(
                string: " · offline",
                attributes: [
                    .font: font,
                    .foregroundColor: Foundry.textMuted,
                    .kern: size * 0.18,
                    .paragraphStyle: paragraph,
                ]))
        }

        let drawn = text.size()
        let origin = NSPoint(x: bounds.midX - drawn.width / 2, y: bounds.midY - drawn.height / 2)
        text.draw(at: origin)
    }

    // MARK: - WKNavigationDelegate

    public func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // stopAnimation()'s teardown navigation also lands here; treating it as a
        // successful console load would put a blank page on screen.
        guard webView.url?.absoluteString != "about:blank" else { return }
        state = .live
        retryTimer?.invalidate()
        retryTimer = nil
        webView.isHidden = false
        needsDisplay = true
        os_log("didFinish url=%{public}@ title=%{public}@",
               log: saverLog, type: .info,
               webView.url?.absoluteString ?? "<nil>", webView.title ?? "<nil>")
    }

    public func webView(_ webView: WKWebView,
                        didFailProvisionalNavigation navigation: WKNavigation!,
                        withError error: Error) {
        enterOffline("didFailProvisionalNavigation " + Self.describe(error as NSError))
    }

    public func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        enterOffline("didFail " + Self.describe(error as NSError))
    }

    public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        enterOffline("WebContent process terminated")
    }

    private static func describe(_ error: NSError) -> String {
        "domain=\(error.domain) code=\(error.code) desc=\(error.localizedDescription)"
    }
}
