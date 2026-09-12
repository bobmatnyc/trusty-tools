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
//   The web view is sized from `bounds` on every host-driven resize and again on
//   the way into `startAnimation()`, so a preview-sized or 0x0 init frame never
//   survives to the first paint (#6871). While the page is live the view asks it
//   every 10 s whether WebKit still considers it visible, because the OS can
//   discard the page's layers without terminating its process (#7112). After
//   three consecutive attempts fail to put a live page on screen, the web view
//   itself is torn down and rebuilt, because a reload cannot leave the frozen
//   WebContent process a fresh one replaces (#7606).
// Test: `LoadHarness.swift` in this directory resolves the principal class,
//   instantiates the view outside the screen-saver host and asserts `didFinish`
//   fires; `PaintHarness.swift` reads the rendered bitmap in the offline,
//   slow-daemon and preview states, tracks the web view's frame across a late
//   host resize in the `resize` state, in the `stop` state asserts the host's
//   stop request is honoured at once (#6900), and in the `suspend` and
//   `suspend-cold` states asserts a re-entrant `startAnimation()` reloads
//   nothing, a page that goes from visible to hidden is reloaded, and a page
//   hidden from its first answer under an unoccluded window is reloaded too —
//   without either reloading a healthy page (#7112), and in the `recreate` and
//   `one-failure` states asserts three consecutive failed loads replace the web
//   view instance while one does not (#7606). The in-host run is manual — see
//   README.md, "Manual verification".
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
        /// #6900: the host has asked the saver to stop. Terminal until the next
        /// `startAnimation()` — nothing may start a load, arm a retry, or put
        /// the web view back on screen from here. `.offline` cannot serve this
        /// purpose because it is the state [`scheduleRetryTimer`] reads as
        /// "keep trying".
        case stopped
        /// #7112: the page was live, and then WebKit stopped treating it as
        /// visible — RunningBoard marks the WebContent process NotVisible and
        /// WebKit runs `freezeAllLayerTrees` → `destroyRenderingResources` →
        /// `markAllLayersVolatile`, discarding the backing store WITHOUT
        /// terminating the process. Distinct from `.offline`, which means the
        /// console is unreachable and draws the OFFLINE banner to say so; here
        /// the console is fine and the reload is already in flight, so the view
        /// shows the dimmed preview with no banner, the way `.stopped` does.
        case suspended
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
    /// #7606: how long one load attempt may stay outstanding before this view
    /// gives up on it. Named once so the retry cadence below cannot drift back
    /// under it — see [`retryGap`].
    private static let loadDeadline: TimeInterval = loadTimeout + loadWatchdogGrace
    /// #7606: how long after that deadline the next attempt starts. Any positive
    /// value satisfies the invariant this constant exists for; two seconds keeps
    /// a console that comes back mid-cycle picked up quickly.
    private static let retryGap: TimeInterval = 2
    /// Retry cadence for the first [`fastRetryWindow`] of an outage.
    ///
    /// #7606: DERIVED from [`loadDeadline`], not chosen. At a flat 5 s it sat
    /// UNDER the 6 s deadline, so every retry issued a `load` over an attempt
    /// WebKit had not finished with; WebKit reported that supersession as
    /// `NSURLErrorCancelled` (-999), [`enterOffline`] read its own cancellation
    /// as a fresh failure and armed another 5 s retry, and the loop fed itself.
    /// The owner's log carried 139 of those in two hours.
    private static let fastRetryInterval: TimeInterval = loadDeadline + retryGap
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
    /// #7112: how often the live page is asked whether WebKit still considers it
    /// visible. Ten seconds is well under the ~20 s rotation the dashboard runs
    /// on, so at most one rotation frame is lost to a suspension.
    private static let visibilityProbeInterval: TimeInterval = 10
    /// #7112: how long that question may go unanswered before the WebContent
    /// process counts as frozen rather than merely slow. JavaScript on a healthy
    /// loopback page answers in single-digit milliseconds.
    private static let visibilityProbeDeadline: TimeInterval = 3
    /// #7112: minimum gap between two visibility recoveries. A host that keeps
    /// the page permanently non-visible would otherwise reload the console every
    /// probe interval; one reload a minute is the ceiling.
    private static let recoveryCooldown: TimeInterval = 60
    /// #7112: how long the view may go on reading an unhealthy page before it
    /// recovers anyway, whatever the other guards say. Without it the "page has
    /// never reported visible" branch is unbounded and [`reloadInterval`] is the
    /// only exit — an hour of the black screen this issue is about.
    private static let forcedRecoveryDeadline: TimeInterval = 60
    /// #7606: consecutive failed attempts to get a live page on screen after
    /// which the `WKWebView` itself is torn down and rebuilt. Three, because a
    /// single failure is an outage the retry backoff already answers and this
    /// costs a WebContent and a network XPC child; three consecutive ones say
    /// the processes behind THIS web view are the thing that is wrong.
    private static let recreateAfterFailures = 3
    /// #7606: minimum gap between two rebuilds while the console has been
    /// reachable recently. Matched to [`recoveryCooldown`] deliberately: a
    /// rebuild is the heavier form of the same remedy and must not run at a
    /// tighter rate than the reload it escalates from.
    private static let recreateCooldown: TimeInterval = 60
    /// #7606: the same gap once the console has been down longer than
    /// [`fastRetryWindow`]. A dead daemon can fail every attempt all night, and
    /// nothing about spawning WebContent processes at the 60 s floor would make
    /// it answer.
    private static let slowRecreateCooldown: TimeInterval = 600

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
    /// #7112: repeating probe of the live page's visibility, armed on `didFinish`
    /// and invalidated on every exit from `.live`.
    private var visibilityTimer: Timer?
    /// #7112: one-shot deadline for the probe in flight. Non-nil means a probe is
    /// outstanding, which is also what keeps two from stacking.
    private var probeDeadlineTimer: Timer?
    /// #7112: whether THIS page instance has ever answered the probe with
    /// `visible`. A page that reports itself hidden from its first probe was
    /// never on screen to begin with, so reloading it would only produce a
    /// treadmill; the transition from visible to hidden is the reported defect.
    /// Reset on every `didFinish`.
    private var sawVisiblePage = false
    /// #7112: when the last visibility recovery started, for [`recoveryCooldown`].
    private var lastRecoveryAt: Date?
    /// #7112: when the current run of unhealthy probe answers began, for
    /// [`forcedRecoveryDeadline`]. Cleared by a healthy answer and by `didFinish`.
    private var unhealthySince: Date?
    /// #7112: whether this view's window last reported itself occluded. An
    /// unoccluded window means the saver IS on screen, so a page answering
    /// `hidden` there is unambiguous and needs no prior-visible history to
    /// interpret. Defaults to false — a view with no window yet is treated as on
    /// screen, because the cost of recovering wrongly is one reload and the cost
    /// of not recovering is a black display.
    private var windowIsOccluded = false
    /// #7112: the window-occlusion observer, kept so it can be removed. Its only
    /// jobs are to log the transition an operator needs to correlate against
    /// RunningBoard's own rows, and to re-probe the instant the window comes back.
    private var occlusionObserver: NSObjectProtocol?
    /// #7606: when the load in flight started; `nil` when none is. What lets
    /// [`abandonInFlightLoad`] tell a load it must cancel from one that already
    /// ended, so `expectingCancellation` is never set speculatively.
    private var loadStartedAt: Date?
    /// #7606: whether the next `NSURLErrorCancelled` belongs to a load THIS view
    /// abandoned. Without it a cancellation the view asked for arrives at
    /// [`webView(_:didFailProvisionalNavigation:withError:)`] indistinguishable
    /// from a real network failure and is counted as one.
    private var expectingCancellation = false
    /// #7606: failed attempts to get a live page on screen since the last one
    /// that provably worked. Drives [`recreateAfterFailures`].
    private var consecutiveLoadFailures = 0
    /// #7606: when the web view was last rebuilt, for the recreate cooldown.
    private var lastRecreateAt: Date?
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

        // #6871: `frame` here is the initializer's PARAMETER, which shadows the
        // property — the frame the host actually handed this view, before any
        // resize. That is the number a mis-sized-on-ultrawide report needs.
        // `.default` rather than `.info` so `log show` persists it after the
        // fact; `.info` entries are memory-only and gone by the time an operator
        // files the bug.
        os_log("init frame=%{public}@ isPreview=%{public}@ url=%{public}@",
               log: saverLog, type: .default,
               NSStringFromRect(frame), String(describing: isPreview), config.url.absoluteString)

        // The System Settings thumbnail is a few hundred pixels of a dashboard
        // nobody can read, and spinning up a WebContent XPC child for it is pure
        // cost. #6839: preview draws the bundled render of that dashboard
        // instead, which is what the tile was missing.
        guard !isPreview else { return }
        webView = makeWebView()
    }

    /// Why: #7606 rebuilds the web view at runtime, and two copies of this
    ///   configuration would drift — a rebuilt view that drew its own background
    ///   or arrived unhidden would look like a different bug entirely.
    /// What: one hidden, full-bounds, delegate-attached `WKWebView`, added as a
    ///   subview. The caller owns the [`webView`] reference.
    /// Test: `PaintHarness.swift`'s `recreate` mode asserts the rebuilt instance
    ///   loads and paints the same way the original did.
    private func makeWebView() -> WKWebView {
        let web = WKWebView(frame: bounds, configuration: WKWebViewConfiguration())
        web.autoresizingMask = [.width, .height]
        web.navigationDelegate = self
        web.setValue(false, forKey: "drawsBackground")
        web.isHidden = true
        addSubview(web)
        return web
    }

    @available(*, unavailable)
    public required init?(coder: NSCoder) {
        fatalError("init(coder:) is not used by the screen-saver host")
    }

    deinit {
        retryTimer?.invalidate()
        reloadTimer?.invalidate()
        loadTimer?.invalidate()
        // #7112: the probe timers and the occlusion observer both retain a
        // reference back into AppKit's notification centre and the run loop.
        visibilityTimer?.invalidate()
        probeDeadlineTimer?.invalidate()
        if let occlusionObserver {
            NotificationCenter.default.removeObserver(occlusionObserver)
        }
        webView?.navigationDelegate = nil
        webView?.removeFromSuperview()
        webView = nil
    }

    // MARK: - ScreenSaverView

    /// Why: on macOS 26 the screen-saver host is `WallpaperAgent`, which calls
    ///   this repeatedly — 20 s to 3.5 min apart — on a view that is already
    ///   running, with no `stopAnimation()` in between. Before #7112 every one of
    ///   those calls reset `state` to `.offline` and reloaded, so the dashboard
    ///   restarted from its first rotation frame each time the host asked, and
    ///   the hourly reload timer was re-armed often enough never to fire.
    /// What: sizes the web view from `bounds`, then returns early when the page
    ///   is already live and on screen. A cold start, a restart after
    ///   `stopAnimation()`, and a restart after the page went `.offline` or
    ///   `.suspended` all fall through to the load as before.
    /// Test: `PaintHarness.swift`'s `suspend` mode calls this three times over a
    ///   live page and asserts the endpoint sees no further connection.
    public override func startAnimation() {
        super.startAnimation()
        // #6871: the host may have constructed this view at a preview size or at
        // 0x0 and supplied the screen's real bounds only afterwards. Size the web
        // view from `bounds` before the load, so no init frame reaches the first
        // paint.
        webView?.frame = bounds
        // #6871: `.default` so `log show` persists it — this line and the `init`
        // one together say whether the host ever handed the view the real screen.
        os_log("startAnimation bounds=%{public}@", log: saverLog, type: .default, NSStringFromRect(bounds))
        // #6838: ask for the fallback on the way in rather than waiting for the
        // first animation tick, so the very first frame the host composites is
        // already the preview and never an unpainted view.
        needsDisplay = true
        guard state != .preview else { return }
        observeOcclusion()
        // #7112: a re-entrant call over a live page reloads nothing. `isHidden`
        // is the second half of the test because it is what `didFinish` clears
        // and every exit from `.live` sets — a `.live` view whose web view is
        // hidden is a state this file does not produce, and reloading is the
        // right answer if a future change ever does.
        if state == .live, webView?.isHidden == false {
            os_log("re-entrant startAnimation ignored — page already live",
                   log: saverLog, type: .default)
            return
        }
        // #7112: a recovery reload is already in flight, with its own watchdog
        // behind it. Restarting would flash the OFFLINE banner over a reachable
        // console and clear `lastRecoveryAt`, which is what caps the reload rate.
        if state == .suspended {
            os_log("re-entrant startAnimation ignored — recovery already in flight",
                   log: saverLog, type: .default)
            return
        }
        // #6900: leaving `.stopped` is this method's job alone. A host that
        // stops the saver and re-arms it — a wake that does not unlock — has to
        // get a loading view back, and the navigation delegate `stopAnimation()`
        // detached on the way out has to come back with it.
        state = .offline
        sawVisiblePage = false
        lastRecoveryAt = nil
        unhealthySince = nil
        // #7606: the failure count and the rebuild cooldown are deliberately NOT
        // reset here. `WallpaperAgent` re-arms a running view every 20 s to
        // 3.5 min, and an offline view falls through to the load below on every
        // one of those calls — clearing the count there would stop it ever
        // reaching the threshold, and clearing the cooldown would let the host's
        // cadence drive the rebuild rate. `stopAnimation()` owns both resets,
        // and a view that has never run holds their initial values anyway.
        webView?.navigationDelegate = self
        loadConsole()
        scheduleReloadTimer()
    }

    /// Why: `autoresizingMask` is a hint that acts only on a superview-driven
    ///   size change, not a contract that the subview matches `bounds`. The
    ///   modern `WallpaperLegacyExtension` host can hand
    ///   `init(frame:isPreview:)` a preview-sized or 0x0 frame and reach the real
    ///   screen bounds by another route, and a web view left at the init frame
    ///   draws the dashboard at the wrong size — reported on a 3440x1440
    ///   ultrawide (#6871).
    /// What: lets autoresizing run, then reasserts the web view's frame from
    ///   `bounds`, which is the only authority on how much screen this view owns.
    ///   A no-op in preview, which builds no web view.
    /// Test: `PaintHarness.swift`'s `resize` mode instantiates the view small,
    ///   grows it to the target frame, and asserts `webView.frame == bounds`.
    public override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        webView?.frame = bounds
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

    /// Why: `loginwindow` asks the screen-saver host to stop and then waits for
    ///   it before handing the display to the unlock UI, so whatever this view
    ///   is still doing after the stop delays Touch ID at unlock (#6900). Before
    ///   #6900 the stop set `state = .offline` — the exact state
    ///   [`scheduleRetryTimer`] reads as "keep trying" — while leaving the
    ///   navigation delegate attached and the in-flight console load running.
    ///   The `about:blank` navigation below then superseded that load, WebKit
    ///   delivered the cancellation to [`webView(_:didFailProvisionalNavigation:withError:)`],
    ///   and [`enterOffline`] re-armed the retry timer the stop had just
    ///   invalidated. From there the loop sustained itself: retry, load,
    ///   watchdog, retry. The saver went on loading the console for four
    ///   minutes after being told to stop.
    /// What: enters the terminal `.stopped` state, cancels the in-flight load,
    ///   and detaches the delegate BEFORE navigating away, so a late WebKit
    ///   callback reaches nothing. Every timer callback and delegate method
    ///   refuses to act in `.stopped`, and only `startAnimation()` leaves it.
    ///   Returns synchronously; the elapsed time is logged so a real unlock can
    ///   be measured against #6900's 500 ms bar without instrumenting the host.
    /// Test: `PaintHarness.swift`'s `stop` mode asserts the call returns inside
    ///   that budget and that no connection reaches the console afterwards —
    ///   both for a stop that lands on an in-flight load and for one that lands
    ///   inside a render tick.
    public override func stopAnimation() {
        let startedAt = Date()
        super.stopAnimation()
        retryTimer?.invalidate()
        retryTimer = nil
        reloadTimer?.invalidate()
        reloadTimer = nil
        loadTimer?.invalidate()
        loadTimer = nil
        offlineSince = nil
        // #7112: the probe and the occlusion observer are two more things the
        // view must not still be doing after the host's stop.
        cancelVisibilityProbe()
        sawVisiblePage = false
        lastRecoveryAt = nil
        unhealthySince = nil
        // #7606: nothing may rebuild the web view after the host's stop, and the
        // cancellation the `about:blank` navigation below produces belongs to no
        // attempt this view is still counting.
        loadStartedAt = nil
        expectingCancellation = false
        consecutiveLoadFailures = 0
        lastRecreateAt = nil
        if let occlusionObserver {
            NotificationCenter.default.removeObserver(occlusionObserver)
            self.occlusionObserver = nil
        }
        guard state != .preview else { return }
        // #6900: the state flip comes FIRST. Everything below can hand control
        // back to WebKit, and every re-entry guard in this file reads `.stopped`.
        state = .stopped
        // #6900: detach before cancelling. A superseded provisional navigation
        // is delivered to the delegate, and on the way out of the saver that
        // callback must have nothing to re-arm.
        webView?.navigationDelegate = nil
        webView?.stopLoading()
        webView?.isHidden = true
        // about:blank tears down the SPA, which stops its metrics polling. Without
        // this the console keeps being polled by an off-screen saver.
        if let blank = URL(string: "about:blank") {
            webView?.load(URLRequest(url: blank))
        }
        // `.default` rather than `.info`: this is the number #6900's acceptance
        // is stated in, and `log show` has to still carry it after an unlock.
        os_log("stopAnimation — stopped in %{public}@ms", log: saverLog, type: .default,
               String(format: "%.1f", Date().timeIntervalSince(startedAt) * 1000))
    }

    /// Why: the fallback has to render even when no web view exists (preview) or the
    ///   web view is hidden (offline), and a transparent screen saver is
    ///   indistinguishable from a crashed one.
    /// What: fills the Foundry dark background, then draws the bundled dashboard
    ///   render unless the live page is on screen — dimmed while offline, and
    ///   banner-stamped there, so a photograph of numbers is never mistaken for
    ///   live ones (#6839). #6900's `.stopped` takes the same dimming for the
    ///   same reason and NOT the banner, which says "retrying" and would be
    ///   false after the host's stop. #7112's `.suspended` takes the same pair:
    ///   the console is reachable and the reload is already in flight, so the
    ///   banner would be false there too. Only the gallery tile draws at full
    ///   brightness. Falls back to the text wordmark if the asset is missing.
    /// Test: `PaintHarness.swift`, all modes.
    public override func draw(_ rect: NSRect) {
        Foundry.background.setFill()
        rect.fill()
        guard state != .live else { return }
        guard drawPreviewAsset(fraction: state == .preview ? 1 : Self.offlineAssetFraction) else {
            drawWordmark()
            return
        }
        if state == .offline { drawOfflineBanner() }
    }

    public override var hasConfigureSheet: Bool { false }
    public override var configureSheet: NSWindow? { nil }

    // MARK: - Loading

    private func loadConsole() {
        // #6900: no load is started after the host's stop, whichever timer or
        // callback got here.
        guard let webView, state != .stopped else { return }
        // #7112: the page about to be replaced is not the page to probe. Without
        // this the hourly reload of a `.live` page leaves a probe in flight, its
        // `evaluateJavaScript` fails on the navigation, and that failure reads as
        // a suspension and issues a second, redundant reload. `didFinish` re-arms.
        cancelVisibilityProbe()
        // #7606: a `load` over an attempt WebKit has not finished with is a
        // supersession, and WebKit reports it as -999. Cancelling here rather
        // than letting the new request do it implicitly is what makes that -999
        // attributable: every one this view can provoke passes through
        // [`abandonInFlightLoad`] first and is recognised on the way back.
        abandonInFlightLoad()
        os_log("loading %{public}@", log: saverLog, type: .info, config.url.absoluteString)
        var request = URLRequest(url: config.url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        // #6838: without this the request inherits URLRequest's 60 s default.
        request.timeoutInterval = Self.loadTimeout
        loadStartedAt = Date()
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
        let deadline = Self.loadDeadline
        loadTimer = Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.loadTimer = nil
            guard self.state != .live else { return }
            // #7606: giving up on an attempt means cancelling it. Left running,
            // it stays outstanding until the next `load` supersedes it, and the
            // -999 that comes back then is indistinguishable from a real one.
            self.abandonInFlightLoad()
            self.enterOffline("load did not finish within \(Int(deadline))s")
        }
    }

    /// Why: the view abandons a load in two places — the watchdog above and the
    ///   #7606 rebuild — and WebKit answers both by delivering
    ///   `NSURLErrorCancelled` to the navigation delegate. Read as a network
    ///   failure that error arms another retry, and the retry abandons another
    ///   load: the -999 treadmill the owner's log carried 139 times in two hours.
    /// What: cancels the attempt in flight, if there is one, and records that the
    ///   cancellation coming back is this view's own doing. A no-op when nothing
    ///   is outstanding, so the flag is never set speculatively — a -999 with no
    ///   abandonment behind it came from outside and is still counted.
    /// Test: `PaintHarness.swift`'s `recreate` mode counts the failures the view
    ///   reaches before it rebuilds; a self-inflicted -999 doubles that count and
    ///   trips the rebuild early.
    private func abandonInFlightLoad() {
        guard let startedAt = loadStartedAt else { return }
        loadStartedAt = nil
        expectingCancellation = true
        webView?.stopLoading()
        os_log("abandoned the load in flight after %{public}@s",
               log: saverLog, type: .info,
               String(format: "%.1f", Date().timeIntervalSince(startedAt)))
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
    ///   [`fastRetryWindow`] of an outage, then [`slowRetryInterval`]. Both
    ///   exceed [`loadDeadline`], so the attempt this fires is never the thing
    ///   that cancels the attempt before it (#7606).
    ///   Invalidating first makes two stacked timers unreachable. The fired
    ///   closure reloads only in `.offline`, which is what keeps `.stopped`
    ///   terminal (#6900) — a timer already in flight when the host stops runs
    ///   out and reloads nothing.
    /// Test: `PaintHarness.swift`'s `slow` mode; `stop` mode for the `.stopped`
    ///   half.
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
        // #6900: a callback that lands after the host's stop must not put the
        // view back into the retrying state the stop just left. This is the
        // exact re-entry that kept the saver loading for four minutes past
        // `loginwindow`'s stop request.
        guard state != .stopped else {
            os_log("ignored after stop — %{public}@", log: saverLog, type: .info, reason)
            return
        }
        state = .offline
        webView?.isHidden = true
        loadTimer?.invalidate()
        loadTimer = nil
        loadStartedAt = nil
        // #7112: there is no live page left to probe.
        cancelVisibilityProbe()
        // Only the FIRST failure of a run sets the clock, so the backoff widens
        // across a long outage instead of resetting on every attempt.
        if offlineSince == nil { offlineSince = Date() }
        needsDisplay = true
        os_log("offline — %{public}@", log: saverLog, type: .error, reason)
        scheduleRetryTimer()
        // #7606: last, so the retry above is armed as a backstop whether or not
        // the rebuild runs. A rebuild that does run loads immediately and that
        // load re-arms the timer from its own outcome.
        noteLoadFailure(reason)
    }

    // MARK: - Web-view rebuild (#7606)

    /// Why: reloading inside a `WKWebView` whose WebContent process the OS has
    ///   parked cannot undo the parking — the reload lands in the same process,
    ///   the page comes back `hidden`, and #7112's recovery reloads it again. The
    ///   owner's saver ran that loop for three days: the console answered
    ///   `/ui/screensaver` in under a millisecond throughout while the screen
    ///   showed the bundled offline preview.
    /// What: counts consecutive failed attempts to get a live page on screen and,
    ///   at [`recreateAfterFailures`], schedules the rebuild. Judged on the
    ///   COUNT, never on what the page said about its own visibility: a page
    ///   reporting itself hidden inside a saver the host is animating is not
    ///   evidence of occlusion, which is the inference that let this run for
    ///   days. Returns whether a rebuild is now on its way, so a caller that
    ///   would otherwise reload can stand down.
    /// Test: `PaintHarness.swift`'s `recreate` mode asserts the instance is
    ///   replaced after three failures and a fresh load follows; its `one-failure`
    ///   mode asserts a single failure replaces nothing.
    @discardableResult
    private func noteLoadFailure(_ reason: String) -> Bool {
        guard state != .stopped, state != .preview else { return false }
        consecutiveLoadFailures += 1
        guard consecutiveLoadFailures >= Self.recreateAfterFailures else { return false }

        let cooldown = currentRecreateCooldown()
        if let lastRecreateAt, Date().timeIntervalSince(lastRecreateAt) < cooldown {
            os_log("rebuild held off — %{public}@ consecutive failure(s), inside the %{public}@s cooldown (%{public}@)",
                   log: saverLog, type: .info,
                   String(consecutiveLoadFailures), String(Int(cooldown)), reason)
            return false
        }

        let failures = consecutiveLoadFailures
        // Claimed BEFORE the rebuild runs, so a second failure landing in the
        // same turn of the run loop cannot queue a second one behind it.
        lastRecreateAt = Date()
        consecutiveLoadFailures = 0
        // Every caller reaches this from a navigation-delegate callback or from a
        // timer that ran one, so WebKit is on the stack. Releasing a `WKWebView`
        // under its own callback is not a supported teardown; the next turn is.
        DispatchQueue.main.async { [weak self] in
            self?.recreateWebView(reason, afterFailures: failures)
        }
        return true
    }

    /// The cooldown in force right now. A console that has been down longer than
    /// [`fastRetryWindow`] fails every attempt by definition, and spawning
    /// WebContent children at the 60 s floor all night would not make it answer.
    private func currentRecreateCooldown() -> TimeInterval {
        let downFor = offlineSince.map { Date().timeIntervalSince($0) } ?? 0
        return downFor < Self.fastRetryWindow ? Self.recreateCooldown : Self.slowRecreateCooldown
    }

    /// Why: a fresh `WKWebView` is a fresh WebContent and network process, which
    ///   is the only thing that clears a page the OS has frozen in place. This is
    ///   the escalation #7112's reload could not reach.
    /// What: detaches the old web view's delegate BEFORE tearing it down — the
    ///   #6900 ordering, for the same reason, so a callback from the process
    ///   being discarded reaches nothing — swaps in a replacement from
    ///   [`makeWebView`], and loads. The state is left as the caller set it, so a
    ///   rebuild reached from `.suspended` keeps the dimmed no-banner frame and
    ///   one reached from `.offline` keeps the OFFLINE banner.
    /// Test: `PaintHarness.swift`'s `recreate` mode.
    private func recreateWebView(_ reason: String, afterFailures failures: Int) {
        // Re-checked rather than assumed: this runs a turn of the run loop after
        // the decision, and the host may have stopped the saver in between.
        guard state != .stopped, state != .preview else {
            os_log("rebuild abandoned — the host stopped the saver first", log: saverLog, type: .info)
            return
        }
        os_log("rebuilding the web view — %{public}@ consecutive failed load(s): %{public}@",
               log: saverLog, type: .default, String(failures), reason)

        cancelVisibilityProbe()
        loadTimer?.invalidate()
        loadTimer = nil
        loadStartedAt = nil
        expectingCancellation = false
        sawVisiblePage = false
        unhealthySince = nil

        if let old = webView {
            old.navigationDelegate = nil
            old.stopLoading()
            old.removeFromSuperview()
        }
        webView = makeWebView()
        needsDisplay = true
        loadConsole()
    }

    // MARK: - Visibility recovery (#7112)

    /// Why: `webViewWebContentProcessDidTerminate` was the view's ONLY exit from
    ///   `.live`, and the failure #7112 reports never terminates anything.
    ///   RunningBoard moves the WebContent process to `running-suspended-NotVisible`
    ///   and WebKit discards its layers; the process stays alive, no delegate
    ///   method fires, `draw(_:)` goes on deferring to a compositor with nothing
    ///   left to composite, and the screen is black.
    /// What: while `.live`, asks the page every [`visibilityProbeInterval`] what
    ///   `document.visibilityState` says. That property is WebKit's own view of
    ///   whether the page is visible and is set by the same activity-state change
    ///   that triggers `freezeAllLayerTrees`, so it reports the cause rather than
    ///   the symptom. An answer of anything but `visible`, or no answer inside
    ///   [`visibilityProbeDeadline`], leaves `.live` and reloads.
    /// Test: `PaintHarness.swift`'s `suspend` mode serves a page that reports
    ///   `visible` once and `hidden` afterwards, and asserts the view reloads.
    private func armVisibilityProbe() {
        visibilityTimer?.invalidate()
        visibilityTimer = Timer.scheduledTimer(withTimeInterval: Self.visibilityProbeInterval,
                                               repeats: true) { [weak self] _ in
            self?.probeVisibility()
        }
    }

    private func cancelVisibilityProbe() {
        visibilityTimer?.invalidate()
        visibilityTimer = nil
        probeDeadlineTimer?.invalidate()
        probeDeadlineTimer = nil
    }

    private func probeVisibility() {
        guard state == .live, let webView else { return }
        // A non-nil deadline means a probe is already outstanding. Stacking a
        // second one would let a slow answer to the first satisfy the second.
        guard probeDeadlineTimer == nil else { return }

        probeDeadlineTimer = Timer.scheduledTimer(withTimeInterval: Self.visibilityProbeDeadline,
                                                  repeats: false) { [weak self] _ in
            guard let self else { return }
            self.probeDeadlineTimer = nil
            // A frozen WebContent process cannot answer, and unlike a `hidden`
            // reading this needs no history to interpret: nothing else in this
            // view leaves JavaScript unevaluated for three seconds.
            self.recoverVisibility("web content did not answer in \(Int(Self.visibilityProbeDeadline))s",
                                   requiresPriorVisible: false)
        }

        webView.evaluateJavaScript("document.visibilityState") { [weak self] value, error in
            guard let self, self.probeDeadlineTimer != nil else { return }
            self.probeDeadlineTimer?.invalidate()
            self.probeDeadlineTimer = nil
            if let answer = value as? String, answer == "visible" {
                self.sawVisiblePage = true
                self.unhealthySince = nil
                // #7606: the ONE signal that a load actually worked. `didFinish`
                // is not it — every reload in the owner's three-day loop finished,
                // and the page it produced was hidden each time. A page that
                // answers `visible` is on screen, and only that clears the count.
                self.consecutiveLoadFailures = 0
                return
            }
            let answer = (value as? String)
                ?? error.map { "error " + Self.describe($0 as NSError) }
                ?? "<nil>"
            self.recoverVisibility("document.visibilityState=\(answer)", requiresPriorVisible: true)
        }
    }

    /// Why: an unhealthy probe alone does not say the page is broken — a saver
    ///   whose window really is behind something SHOULD have a hidden page.
    ///   Deciding on `sawVisiblePage` alone failed open: that flag resets on
    ///   every `didFinish`, so a page suspended inside its first probe interval
    ///   answered `hidden` with no history, only logged, and stayed `.live` with
    ///   the web view on screen — which the re-entrant guard then read as
    ///   healthy, leaving [`reloadInterval`] as the only exit. An hour of #7112.
    /// What: recovers on any ONE of four grounds, and every path out is bounded:
    ///   the saver's own window is not occluded, so a hidden page contradicts
    ///   what is on screen; a visible-then-hidden transition was observed; the
    ///   page stopped answering at all; or the page has been unhealthy for
    ///   [`forcedRecoveryDeadline`]. [`recoveryCooldown`] caps the reload rate.
    ///   The trigger's name is in the log line so `log show` can tell this apart
    ///   from the re-entrant-`startAnimation` path and from an ordinary outage.
    /// Test: `PaintHarness.swift`'s `suspend` mode drives the transition ground,
    ///   `suspend-cold` the unoccluded-window one.
    private func recoverVisibility(_ reason: String, requiresPriorVisible: Bool) {
        guard state == .live else { return }
        if unhealthySince == nil { unhealthySince = Date() }
        let unhealthyFor = unhealthySince.map { Date().timeIntervalSince($0) } ?? 0

        let ground: String
        if !requiresPriorVisible {
            ground = "no answer"
        } else if !windowIsOccluded {
            ground = "window not occluded"
        } else if sawVisiblePage {
            ground = "was visible"
        } else if unhealthyFor >= Self.forcedRecoveryDeadline {
            ground = "unhealthy for \(Int(unhealthyFor))s"
        } else {
            // The one branch that waits: an occluded window whose page has never
            // reported visible is a page that was plausibly never on screen.
            // Bounded by the deadline above, so this cannot outlive a minute.
            os_log("visibility lost, waiting — occluded window, page never reported visible (%{public}@)",
                   log: saverLog, type: .default, reason)
            return
        }

        if let lastRecoveryAt, Date().timeIntervalSince(lastRecoveryAt) < Self.recoveryCooldown {
            os_log("visibility lost, inside the recovery cooldown — %{public}@",
                   log: saverLog, type: .info, reason)
            return
        }
        lastRecoveryAt = Date()
        state = .suspended
        // The dimmed preview goes up NOW rather than after the reload answers,
        // so the discarded layer is never what the screen is showing.
        webView?.isHidden = true
        needsDisplay = true
        os_log("visibility lost, recovering — %{public}@ (%{public}@)",
               log: saverLog, type: .default, reason, ground)
        // #7606: a recovery is a failed attempt to keep a live page on screen,
        // and counts as one. Three in a row mean the reload is not working and
        // the process behind the page goes instead — the rebuild loads for us.
        if noteLoadFailure("visibility recovery: \(reason)") { return }
        loadConsole()
    }

    /// Why: #7112 could not time the view's `.live` state against the OS marking
    ///   its web content NotVisible, because the view logged nothing about
    ///   visibility at all.
    /// What: logs this view's window's occlusion transitions at `.default` so
    ///   `log show` carries them next to RunningBoard's own rows, and re-probes
    ///   the moment the window is visible again instead of waiting out the rest
    ///   of the interval.
    /// Test: manual — README.md, "Manual verification".
    private func observeOcclusion() {
        guard occlusionObserver == nil else { return }
        // Seed from the window this view is in right now; the notification only
        // reports CHANGES, and a view that never sees one must not be left
        // guessing. No window yet means treated as on screen — see
        // [`windowIsOccluded`].
        windowIsOccluded = window.map { !$0.occlusionState.contains(.visible) } ?? false
        occlusionObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeOcclusionStateNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self,
                  let changed = notification.object as? NSWindow,
                  changed === self.window else { return }
            let visible = changed.occlusionState.contains(.visible)
            self.windowIsOccluded = !visible
            os_log("window occlusion changed — visible=%{public}@",
                   log: saverLog, type: .default, visible ? "true" : "false")
            if visible { self.probeVisibility() }
        }
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
        // #6900: a load that finishes after the host's stop must not unhide the
        // web view or take the view out of `.stopped`.
        guard state != .stopped else { return }
        // stopAnimation()'s teardown navigation also lands here; treating it as a
        // successful console load would put a blank page on screen.
        guard webView.url?.absoluteString != "about:blank" else { return }
        state = .live
        loadStartedAt = nil
        expectingCancellation = false
        retryTimer?.invalidate()
        retryTimer = nil
        loadTimer?.invalidate()
        loadTimer = nil
        // #6838: the next outage starts its own backoff clock, so a console that
        // came back and went again gets fast retries a second time.
        offlineSince = nil
        webView.isHidden = false
        needsDisplay = true
        // #7112: this page instance has not answered a probe yet, and nothing
        // else exits `.live` when the OS discards its layers without killing it.
        sawVisiblePage = false
        unhealthySince = nil
        armVisibilityProbe()
        os_log("didFinish url=%{public}@ title=%{public}@",
               log: saverLog, type: .info,
               webView.url?.absoluteString ?? "<nil>", webView.title ?? "<nil>")
    }

    public func webView(_ webView: WKWebView,
                        didFailProvisionalNavigation navigation: WKNavigation!,
                        withError error: Error) {
        let nsError = error as NSError
        // #7606: a cancellation this view asked for in [`abandonInFlightLoad`] is
        // not a failure to count and not a reason to retry — the attempt that
        // replaces it is already accounted for. Counting it is what turned one
        // failure into two and halved the effective retry interval.
        if expectingCancellation, nsError.domain == NSURLErrorDomain, nsError.code == NSURLErrorCancelled {
            expectingCancellation = false
            os_log("ignored own cancellation — %{public}@", log: saverLog, type: .info,
                   Self.describe(nsError))
            return
        }
        expectingCancellation = false
        enterOffline("didFailProvisionalNavigation " + Self.describe(nsError))
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
