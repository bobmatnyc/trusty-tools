// TrustyConsoleSaver — the macOS screen-saver bundle for trusty-console (#6520,
// Phase 4 of epic #6516).
//
// Why: the console already renders a full machine-status dashboard at
//   `/ui/screensaver` (#6519). macOS has no way to run a web page as a screen
//   saver, so the route needs a native `.saver` bundle around it. This file is
//   that wrapper and nothing more — every pixel of the live view comes from the
//   console SPA, so dashboard changes ship without rebuilding the bundle.
// What: a `ScreenSaverView` subclass hosting one full-bounds `WKWebView` pointed
//   at `http://127.0.0.1:<port><path>`, with a bundled static preview of the
//   dashboard as its fallback, a bounded load timeout, a 5 s→30 s retry backoff
//   while the console is down, and an hourly reload for long-run memory hygiene.
// Test: `LoadHarness.swift` in this directory resolves the principal class,
//   instantiates the view outside the screen-saver host and asserts `didFinish`
//   fires; `PaintHarness.swift` reads the rendered bitmap in the offline,
//   slow-daemon and preview states. The in-host run is manual — see README.md,
//   "Manual verification".
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

    /// How long one load attempt may run before it counts as a failure.
    ///
    /// #6838: a console that has BOUND its port during a restart but cannot yet
    /// answer produces no `didFailProvisionalNavigation` — that fires on a
    /// refused connection, which is fast. It produces silence, for
    /// `URLRequest`'s 60 s default, during which nothing moved the view off its
    /// first frame. Five seconds is short enough that the operator sees the
    /// fallback instead of a stall, and long enough that a slow first paint of
    /// the real page is not mistaken for an outage.
    private static let loadTimeout: TimeInterval = 5
    /// How long after [`loadTimeout`] the view's own watchdog fires. The grace
    /// lets WebKit's error callback win the ordinary race, so the log carries
    /// the real `NSError` rather than the watchdog's generic reason.
    private static let loadWatchdogGrace: TimeInterval = 1
    /// Retry cadence for the first [`fastRetryWindow`] of an outage.
    private static let fastRetryInterval: TimeInterval = 5
    /// Retry cadence once the console has been down longer than that.
    private static let slowRetryInterval: TimeInterval = 30
    /// How long retries stay fast. A daemon reinstall is back inside a minute,
    /// so the first few minutes are worth polling hard; an overnight outage is
    /// not, and a screen saver left running must not spend the night issuing
    /// thousands of futile requests.
    private static let fastRetryWindow: TimeInterval = 180
    /// Full reload cadence while animating — long-run memory hygiene, not freshness
    /// (the SPA polls its own data).
    private static let reloadInterval: TimeInterval = 3600

    private var webView: WKWebView?
    private var retryTimer: Timer?
    private var reloadTimer: Timer?
    /// #6838: fires when a load neither finishes nor fails inside
    /// [`loadTimeout`]. Belt to `URLRequest.timeoutInterval`'s braces — WebKit
    /// owns when it honours a request timeout, and this view owns when it stops
    /// waiting.
    private var loadTimer: Timer?
    /// When the current run of failures started; `nil` while the page is live.
    /// Drives which retry cadence [`scheduleRetryTimer`] picks.
    private var offlineSince: Date?
    private var state: DisplayState
    private let config = SaverConfig.current()
    /// #6839: the bundled render of the dashboard, drawn whenever the live page
    /// is not on screen. Lazy because the gallery tile and the live view need it
    /// at different moments and neither wants it read twice.
    private lazy var previewAsset: NSImage? = Self.loadPreviewAsset()

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
        // cost. #6839: preview draws the bundled render of that dashboard
        // instead, which is what the tile was missing.
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
        loadTimer?.invalidate()
        webView?.navigationDelegate = nil
        webView?.removeFromSuperview()
        webView = nil
    }

    // MARK: - ScreenSaverView

    public override func startAnimation() {
        super.startAnimation()
        // #6838: ask for the fallback on the way in rather than waiting for the
        // first animation tick, so the very first frame the host composites is
        // already the preview and never an unpainted view.
        needsDisplay = true
        guard state != .preview else { return }
        loadConsole()
        scheduleReloadTimer()
    }

    /// Why: `ScreenSaverView`'s contract is that the host drives repainting
    ///   through this method on `animationTimeInterval`. Without an override,
    ///   every repaint this view ever performs depends on one of three
    ///   event-driven call sites firing — so a first paint the full-screen host
    ///   drops, or a load that neither finishes nor fails, leaves the screen on
    ///   whatever was there before, which is black (#6838).
    /// What: invalidates once per tick while the live page is not on screen.
    ///   `.live` is skipped because the `WKWebView` draws itself and forcing a
    ///   redraw behind it every second is pure cost.
    /// Test: `PaintHarness.swift` measures the rendered bitmap in the offline
    ///   and slow-daemon modes.
    public override func animateOneFrame() {
        super.animateOneFrame()
        guard state != .live else { return }
        needsDisplay = true
    }

    public override func stopAnimation() {
        super.stopAnimation()
        retryTimer?.invalidate()
        retryTimer = nil
        reloadTimer?.invalidate()
        reloadTimer = nil
        loadTimer?.invalidate()
        loadTimer = nil
        offlineSince = nil
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
    /// What: fills the Foundry dark background, then draws the bundled dashboard
    ///   render unless the live page is on screen — dimmed and banner-stamped
    ///   while offline, so a photograph of numbers is never mistaken for live
    ///   ones (#6839). Falls back to the text wordmark if the asset is missing.
    /// Test: `PaintHarness.swift`, all three modes.
    public override func draw(_ rect: NSRect) {
        Foundry.background.setFill()
        rect.fill()
        guard state != .live else { return }
        guard drawPreviewAsset(fraction: state == .offline ? Self.offlineAssetFraction : 1) else {
            drawWordmark()
            return
        }
        if state == .offline { drawOfflineBanner() }
    }

    public override var hasConfigureSheet: Bool { false }
    public override var configureSheet: NSWindow? { nil }

    // MARK: - Loading

    private func loadConsole() {
        guard let webView else { return }
        os_log("loading %{public}@", log: saverLog, type: .info, config.url.absoluteString)
        var request = URLRequest(url: config.url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        // #6838: without this the request inherits URLRequest's 60 s default.
        request.timeoutInterval = Self.loadTimeout
        webView.load(request)
        // Only while the fallback is what the operator can see. An hourly reload
        // of a page already on screen has its own failure callbacks, and pulling
        // a live dashboard down because one refresh was slow would be worse than
        // the stale frame it replaces.
        if state != .live { startLoadWatchdog() }
    }

    /// Why: WebKit decides for itself whether to honour a request's
    ///   `timeoutInterval`, and #6838 is precisely the case where nothing came
    ///   back at all. This view owns when it stops waiting.
    /// What: one-shot timer; if the load has not reached `.live` by then, the
    ///   view enters the offline state and the retry backoff takes over.
    /// Test: `PaintHarness.swift`'s `slow` mode counts the retries this produces
    ///   against a listener that accepts and never answers.
    private func startLoadWatchdog() {
        loadTimer?.invalidate()
        let deadline = Self.loadTimeout + Self.loadWatchdogGrace
        loadTimer = Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.loadTimer = nil
            guard self.state != .live else { return }
            self.enterOffline("load did not finish within \(Int(deadline))s")
        }
    }

    private func scheduleReloadTimer() {
        reloadTimer?.invalidate()
        reloadTimer = Timer.scheduledTimer(withTimeInterval: Self.reloadInterval, repeats: true) { [weak self] _ in
            guard let self, self.state == .live else { return }
            os_log("hourly reload", log: saverLog, type: .info)
            self.loadConsole()
        }
    }

    /// Why: #6838 asks for a console that comes back to be picked up without the
    ///   saver restarting, which means retrying — but a screen saver left on a
    ///   dead daemon overnight must not retry at 5 s forever.
    /// What: one-shot rather than repeating, rescheduled per attempt, so the
    ///   cadence can widen: [`fastRetryInterval`] for the first
    ///   [`fastRetryWindow`] of an outage, then [`slowRetryInterval`].
    ///   Invalidating first makes two stacked timers unreachable.
    /// Test: `PaintHarness.swift`'s `slow` mode.
    private func scheduleRetryTimer() {
        retryTimer?.invalidate()
        let downFor = offlineSince.map { Date().timeIntervalSince($0) } ?? 0
        let delay = downFor < Self.fastRetryWindow ? Self.fastRetryInterval : Self.slowRetryInterval
        retryTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.retryTimer = nil
            guard self.state == .offline else { return }
            self.loadConsole()
        }
    }

    private func enterOffline(_ reason: String) {
        state = .offline
        webView?.isHidden = true
        loadTimer?.invalidate()
        loadTimer = nil
        // Only the FIRST failure of a run sets the clock, so the backoff widens
        // across a long outage instead of resetting on every attempt.
        if offlineSince == nil { offlineSince = Date() }
        needsDisplay = true
        os_log("offline — %{public}@", log: saverLog, type: .error, reason)
        scheduleRetryTimer()
    }

    // MARK: - Static preview asset (#6839)

    /// Basename of the PNG in `Contents/Resources/`, produced by
    /// `scripts/render-console-saver-preview.sh` and copied in by
    /// `scripts/build-console-saver.sh`.
    private static let previewAssetName = "ConsolePreview"
    /// How much of the asset shows through while offline. Dim enough that a
    /// photograph of last week's numbers cannot pass for live ones, bright
    /// enough that the screen is unmistakably the console and not a fault.
    private static let offlineAssetFraction: CGFloat = 0.35

    private static func loadPreviewAsset() -> NSImage? {
        let bundle = Bundle(for: TrustyConsoleSaverView.self)
        guard let url = bundle.url(forResource: previewAssetName, withExtension: "png"),
              let image = NSImage(contentsOf: url) else {
            os_log("preview asset %{public}@.png missing or unreadable — falling back to the wordmark",
                   log: saverLog, type: .error, previewAssetName)
            return nil
        }
        return image
    }

    /// Draws the asset scaled to fit, centred, at `fraction` opacity.
    /// Returns `false` when there is no usable asset, which is the caller's cue
    /// to draw the text wordmark instead.
    private func drawPreviewAsset(fraction: CGFloat) -> Bool {
        guard let image = previewAsset else { return false }
        let source = image.size
        guard source.width > 0, source.height > 0, bounds.width > 0, bounds.height > 0 else {
            return false
        }
        // Fit, not fill: a screen saver that crops the dashboard's own edges
        // loses the header and the service table's last column.
        let scale = min(bounds.width / source.width, bounds.height / source.height)
        let drawn = NSSize(width: source.width * scale, height: source.height * scale)
        let target = NSRect(x: bounds.midX - drawn.width / 2,
                            y: bounds.midY - drawn.height / 2,
                            width: drawn.width,
                            height: drawn.height)
        image.draw(in: target, from: .zero, operation: .sourceOver, fraction: fraction)
        return true
    }

    /// The "offline" line #6838 asks for, over a scrim so it stays legible
    /// against whatever part of the dashboard sits behind it.
    private func drawOfflineBanner() {
        let size = max(14, bounds.height * 0.035)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center

        let headline = NSAttributedString(
            string: "TRUSTY CONSOLE · OFFLINE",
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: size, weight: .medium),
                .foregroundColor: Foundry.textPrimary,
                .kern: size * 0.18,
                .paragraphStyle: paragraph,
            ])
        let detail = NSAttributedString(
            string: "showing a saved preview — retrying",
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: size * 0.55, weight: .regular),
                .foregroundColor: Foundry.textMuted,
                .kern: size * 0.10,
                .paragraphStyle: paragraph,
            ])

        let headlineSize = headline.size()
        let detailSize = detail.size()
        let stackHeight = headlineSize.height + detailSize.height + size * 0.6
        let band = NSRect(x: bounds.minX,
                          y: bounds.midY - stackHeight,
                          width: bounds.width,
                          height: stackHeight * 2)
        Foundry.background.withAlphaComponent(0.82).setFill()
        band.fill()

        headline.draw(at: NSPoint(x: bounds.midX - headlineSize.width / 2,
                                  y: bounds.midY + size * 0.2))
        detail.draw(at: NSPoint(x: bounds.midX - detailSize.width / 2,
                                y: bounds.midY - size * 0.2 - detailSize.height))
    }

    // MARK: - Native fallback

    /// The text card drawn when the bundled asset is missing — a broken build,
    /// not a state the operator should ever reach.
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
        loadTimer?.invalidate()
        loadTimer = nil
        // #6838: the next outage starts its own backoff clock, so a console that
        // came back and went again gets fast retries a second time.
        offlineSince = nil
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
