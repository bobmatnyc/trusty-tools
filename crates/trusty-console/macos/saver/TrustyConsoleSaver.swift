// TrustyConsoleSaver — the macOS screen-saver bundle for trusty-console (#6520,
// Phase 4 of epic #6516).
//
// Why: the console already renders a full machine-status dashboard at
//   `/ui/screensaver` (#6519). macOS has no way to run a web page as a screen
//   saver, so the route needs a native `.saver` bundle around it. These files
//   are that wrapper and nothing more — every pixel of the live view comes from the
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
//   WebContent process a fresh one replaces (#7606). While the saver's own
//   window is occluded the load deadline WAITS instead of expiring, because
//   WebKit throttles a backgrounded web view and the deadline would otherwise
//   measure that throttling rather than the console (#7846).
// Test: `LoadHarness.swift` in this directory resolves the principal class,
//   instantiates the view outside the screen-saver host and asserts `didFinish`
//   fires; `PaintHarness/` reads the rendered bitmap in the offline,
//   slow-daemon and preview states, tracks the web view's frame across a late
//   host resize in the `resize` state, in the `stop` state asserts the host's
//   stop request is honoured at once (#6900), and in the `suspend` and
//   `suspend-cold` states asserts a re-entrant `startAnimation()` reloads
//   nothing, a page that goes from visible to hidden is reloaded, and a page
//   hidden from its first answer under an unoccluded window is reloaded too —
//   without either reloading a healthy page (#7112), and in the `recreate` and
//   `one-failure` states asserts three consecutive failed loads replace the web
//   view instance while one does not (#7606), and in the `occluded`,
//   `occluded-failing` and `visibility-unknown` states asserts an occluded
//   window neither fails a load nor feeds the rebuild count, that a window
//   coming back restarts the deadline, and that an unknown answer falls back to
//   the ordinary deadline (#7846). The in-host run is manual — see README.md,
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

let saverLog = OSLog(subsystem: "com.trusty.console.saver", category: "saver")

/// Why: the offline and preview states are drawn natively, so they cannot pick up
///   the console's stylesheet. Hardcoding the Foundry dark-theme values keeps the
///   fallback visually continuous with the live dashboard.
/// What: the four `[data-theme='dark']` tokens this view paints with, copied from
///   `docs/design/UI/design-system/tokens.css`.
/// Test: visual — the fallback matches the console's dark background.
enum Foundry {
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
    enum DisplayState {
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
    static let loadTimeout: TimeInterval = 5
    /// How long after [`loadTimeout`] the view's own watchdog fires. The grace
    /// lets WebKit's error callback win the ordinary race, so the log carries
    /// the real `NSError` rather than the watchdog's generic reason.
    private static let loadWatchdogGrace: TimeInterval = 1
    /// #7606: how long one load attempt may stay outstanding before this view
    /// gives up on it. Named once so the retry cadence below cannot drift back
    /// under it — see [`retryGap`].
    static let loadDeadline: TimeInterval = loadTimeout + loadWatchdogGrace
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
    static let fastRetryInterval: TimeInterval = loadDeadline + retryGap
    /// Retry cadence once the console has been down longer than that.
    static let slowRetryInterval: TimeInterval = 30
    /// How long retries stay fast. A daemon reinstall is back inside a minute,
    /// so the first few minutes are worth polling hard; an overnight outage is
    /// not, and a screen saver left running must not spend the night issuing
    /// thousands of futile requests.
    static let fastRetryWindow: TimeInterval = 180
    /// Full reload cadence while animating — long-run memory hygiene, not freshness
    /// (the SPA polls its own data).
    static let reloadInterval: TimeInterval = 3600
    /// #7112: how often the live page is asked whether WebKit still considers it
    /// visible. Ten seconds is well under the ~20 s rotation the dashboard runs
    /// on, so at most one rotation frame is lost to a suspension.
    static let visibilityProbeInterval: TimeInterval = 10
    /// #7112: how long that question may go unanswered before the WebContent
    /// process counts as frozen rather than merely slow. JavaScript on a healthy
    /// loopback page answers in single-digit milliseconds.
    static let visibilityProbeDeadline: TimeInterval = 3
    /// #7112: minimum gap between two visibility recoveries. A host that keeps
    /// the page permanently non-visible would otherwise reload the console every
    /// probe interval; one reload a minute is the ceiling.
    static let recoveryCooldown: TimeInterval = 60
    /// #7112: how long the view may go on reading an unhealthy page before it
    /// recovers anyway, whatever the other guards say. Without it the "page has
    /// never reported visible" branch is unbounded and [`reloadInterval`] is the
    /// only exit — an hour of the black screen this issue is about.
    static let forcedRecoveryDeadline: TimeInterval = 60
    /// #7606: consecutive failed attempts to get a live page on screen after
    /// which the `WKWebView` itself is torn down and rebuilt. Three, because a
    /// single failure is an outage the retry backoff already answers and this
    /// costs a WebContent and a network XPC child; three consecutive ones say
    /// the processes behind THIS web view are the thing that is wrong.
    static let recreateAfterFailures = 3
    /// #7606: minimum gap between two rebuilds while the console has been
    /// reachable recently. Matched to [`recoveryCooldown`] deliberately: a
    /// rebuild is the heavier form of the same remedy and must not run at a
    /// tighter rate than the reload it escalates from.
    static let recreateCooldown: TimeInterval = 60
    /// #7606: the same gap once the console has been down longer than
    /// [`fastRetryWindow`]. A dead daemon can fail every attempt all night, and
    /// nothing about spawning WebContent processes at the 60 s floor would make
    /// it answer.
    static let slowRecreateCooldown: TimeInterval = 600
    /// #7846: how long an occluded view waits before re-asking about its window
    /// and, if the attempt it was waiting on has ended, trying again.
    ///
    /// A minute rather than [`fastRetryInterval`]'s eight seconds, because a
    /// screen nobody can see earns no urgency: the occlusion notification is
    /// what makes a returning window immediate, and this is only the backstop
    /// for a notification that never arrives. The owner's six-hour log is what
    /// the eight-second cadence costs when every attempt is throttled.
    static let occludedRetryInterval: TimeInterval = 60
    /// #7846: how many consecutive re-asks may answer UNKNOWN before the view
    /// stops waiting and judges the load on the ordinary deadline. Waiting on an
    /// answer nobody gave is a fail-open, and a fail-open with no bound is a
    /// saver that waits silently forever; three re-asks at the fast retry
    /// cadence is under half a minute of patience for a view that has not yet
    /// been put in a window.
    static let maxUnknownVisibilityProbes = 3

    var webView: WKWebView?
    var retryTimer: Timer?
    var reloadTimer: Timer?
    /// #6838: fires when a load neither finishes nor fails inside
    /// [`loadTimeout`]. Belt to `URLRequest.timeoutInterval`'s braces — WebKit
    /// owns when it honours a request timeout, and this view owns when it stops
    /// waiting.
    var loadTimer: Timer?
    /// When the current run of failures started; `nil` while the page is live.
    /// Drives which retry cadence [`scheduleRetryTimer`] picks.
    var offlineSince: Date?
    /// #7112: repeating probe of the live page's visibility, armed on `didFinish`
    /// and invalidated on every exit from `.live`.
    var visibilityTimer: Timer?
    /// #7112: one-shot deadline for the probe in flight. Non-nil means a probe is
    /// outstanding, which is also what keeps two from stacking.
    var probeDeadlineTimer: Timer?
    /// #7112: whether THIS page instance has ever answered the probe with
    /// `visible`. A page that reports itself hidden from its first probe was
    /// never on screen to begin with, so reloading it would only produce a
    /// treadmill; the transition from visible to hidden is the reported defect.
    /// Reset on every `didFinish`.
    var sawVisiblePage = false
    /// #7112: when the last visibility recovery started, for [`recoveryCooldown`].
    var lastRecoveryAt: Date?
    /// #7112: when the current run of unhealthy probe answers began, for
    /// [`forcedRecoveryDeadline`]. Cleared by a healthy answer and by `didFinish`.
    var unhealthySince: Date?
    /// #7112: whether this view's window last reported itself occluded. An
    /// unoccluded window means the saver IS on screen, so a page answering
    /// `hidden` there is unambiguous and needs no prior-visible history to
    /// interpret. Defaults to false — a view with no window yet is treated as on
    /// screen, because the cost of recovering wrongly is one reload and the cost
    /// of not recovering is a black display.
    var windowIsOccluded = false
    /// #7112: the window-occlusion observer, kept so it can be removed. Its only
    /// jobs are to log the transition an operator needs to correlate against
    /// RunningBoard's own rows, and to re-probe the instant the window comes back.
    var occlusionObserver: NSObjectProtocol?
    /// #7606: when the load in flight started; `nil` when none is. What lets
    /// [`abandonInFlightLoad`] tell a load it must cancel from one that already
    /// ended, so `expectingCancellation` is never set speculatively.
    var loadStartedAt: Date?
    /// #7606: whether the next `NSURLErrorCancelled` belongs to a load THIS view
    /// abandoned. Without it a cancellation the view asked for arrives at
    /// [`webView(_:didFailProvisionalNavigation:withError:)`] indistinguishable
    /// from a real network failure and is counted as one.
    var expectingCancellation = false
    /// #7606: failed attempts to get a live page on screen since the last one
    /// that provably worked. Drives [`recreateAfterFailures`].
    var consecutiveLoadFailures = 0
    /// #7606: when the web view was last rebuilt, for the recreate cooldown.
    var lastRecreateAt: Date?
    /// #7846: whether this view is waiting for a window to be on screen rather
    /// than judging a load. Spans attempts, because the thing being waited on is
    /// the window and not any one attempt, and it is what entitles the attempt
    /// in flight to a fresh full deadline when the window comes back.
    var waitingForWindow = false
    /// #7846: when the current run of waiting began, so the log says how long
    /// the view has been waiting rather than only that it is.
    var waitingForWindowSince: Date?
    /// #7846: consecutive re-asks answered UNKNOWN, for
    /// [`maxUnknownVisibilityProbes`]. Latches at the bound, so the fall-back
    /// stays fallen back; cleared by any answer either way.
    var unknownVisibilityProbes = 0
    var state: DisplayState
    let config = SaverConfig.current()
    /// #6839: the bundled render of the dashboard, drawn whenever the live page
    /// is not on screen. Lazy because the gallery tile and the live view need it
    /// at different moments and neither wants it read twice.
    lazy var previewAsset: NSImage? = Self.loadPreviewAsset()

    /// #7846: TEST SEAM. Zero — the default — means ask AppKit; any other value
    /// is a forced [`WindowVisibility`] raw value.
    ///
    /// Why: `PaintHarness/` parks its window off every display, and AppKit
    ///   reports no `.visible` for such a window, so every harness mode would
    ///   otherwise run as occluded and the modes that assert today's deadline
    ///   path would stop asserting anything. The occlusion the real host applies
    ///   cannot be produced from a harness either way, so the verdict is the
    ///   seam rather than the window.
    /// What: `@objc` so the harness can reach it by KVC — it loads the principal
    ///   class by name and has no Swift type to cast to.
    /// Test: `PaintHarness/`'s `occluded`, `occluded-failing` and
    ///   `visibility-unknown` modes set it; every other mode leaves it at zero.
    @objc public var windowVisibilityOverride: Int = 0

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
    /// Test: `PaintHarness/`'s `recreate` mode asserts the rebuilt instance
    ///   loads and paints the same way the original did.
    func makeWebView() -> WKWebView {
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
    /// Test: `PaintHarness/`'s `suspend` mode calls this three times over a
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
    /// Test: `PaintHarness/`'s `resize` mode instantiates the view small,
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
    /// Test: `PaintHarness/` measures the rendered bitmap in the offline
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
    /// Test: `PaintHarness/`'s `stop` mode asserts the call returns inside
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
        // #7846: nothing is waiting for a window any more either.
        waitingForWindow = false
        waitingForWindowSince = nil
        unknownVisibilityProbes = 0
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
    /// Test: `PaintHarness/`, all modes.
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
}
