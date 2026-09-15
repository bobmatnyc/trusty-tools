// PaintHarness — the numbers every mode is judged against.
//
// Split from `main.swift` for the 500-SLOC cap (#7856); that file's header
// carries the Why/What/Test for the whole harness.

import Foundation

// MARK: - Thresholds

/// Fraction of pixels that must be brighter than near-black. This is #6838's
/// acceptance restated as a number: a black frame is the bug.
let minNonBlackRatio = 0.98
/// Fraction of pixels that must differ from the flat Foundry background — i.e.
/// something was actually DRAWN. This is what separates "a static preview
/// renders" from "a mostly empty view with a label on it" (#6839).
///
/// 2% sits between the two states it has to tell apart, measured at 1280x800
/// against the bundles this shipped with:
///
/// | mode    | text wordmark (unfixed) | bundled asset (fixed) |
/// |---------|-------------------------|-----------------------|
/// | offline | 0.0034                  | 0.0417                |
/// | slow    | 0.0034                  | 0.0417                |
/// | preview | 0.0022                  | 0.1152                |
///
/// So the bar clears the worst unfixed frame by 5.9x and sits 2.1x under the
/// worst fixed one. The offline number is the tight side because the asset is
/// drawn at 35% there, which divides every source pixel's distance from the
/// background back through that blend. Re-measure before moving it.
///
/// #6871: `--frame` moves the geometry those numbers came from. The fallback
/// asset is drawn to FIT, so a frame wider than the asset's 16:9 letterboxes it
/// and the ink ratio falls — offline 0.0417 → 0.0328 and preview 0.1152 →
/// 0.0942 going from 1280x800 to 3440x1440.
/// The bar is unchanged and still cleared; it is not per-frame.
let minInkRatio = 0.02
/// How long from the view being READY to its first non-black, non-empty frame.
/// #6838's acceptance says one second, and measuring before `startAnimation()`
/// states it more strictly — there is no window at all, not even one frame, in
/// which the view can be black.
///
/// The clock starts when `init(frame:isPreview:)` RETURNS, so it excludes both
/// that call and `startAnimation()`. Both bring WebKit up inside this
/// single-process harness — the non-preview `init` measured 1.32 s and
/// `startAnimation` 1.1 s in observed runs, against 0.18 s for the preview path
/// that builds no web view. That is WebKit's XPC bring-up, which the real
/// screen-saver host pays in a separate service and which never gates the
/// view's own `draw(_:)`. Charging it here would measure the harness.
///
/// This is the weakest of the harness's assertions and is not what separates a
/// fixed bundle from an unfixed one — `draw(_:)` was always fast. The ink ratio
/// and the `slow` mode's retry count are the real gates.
let firstPaintDeadline: TimeInterval = 1.0
/// How long the `slow` mode watches its listener for retry attempts. The fixed
/// view attempts at roughly 0 s, 14 s, 28 s and 42 s — a 5 s request timeout
/// with a 1 s watchdog grace, plus an 8 s retry delay — so this window holds
/// four attempts and demands three.
///
/// #7606 moved the retry delay from 5 s to 8 s and this window from 34 s with
/// it. The cadence is no longer a free number: a retry that lands before the 6 s
/// deadline supersedes the attempt in flight, and WebKit reports that
/// supersession as -999. The view now derives the delay from the deadline.
let retryObservationSeconds: TimeInterval = 45
/// Connection attempts the `slow` mode expects inside that window: the initial
/// load plus at least two retries. Without a request timeout the view issues
/// one connection and waits out `URLRequest`'s 60 s default, so this is the
/// assertion that fails on the unfixed bundle.
let minSlowModeAttempts = 3

// MARK: - Stop-path thresholds (#6900)

/// How long `stopAnimation()` itself may take to return. #6900's acceptance
/// says "under 500 ms"; `loginwindow` waits on the screen-saver host before it
/// hands the display to the unlock UI, so this is what Touch ID waits behind.
///
/// The call does synchronous work only — invalidate four timers, detach a
/// delegate, cancel a load — so the real number is microseconds and the budget
/// is not a tight fit. It is here to catch a future change that puts a blocking
/// wait, a synchronous XPC round trip, or a run-loop spin into the stop path.
let stopReturnBudget: TimeInterval = 0.5
/// How long after the stop the listener is watched for traffic that must not
/// come. The unfixed view's re-armed retry lands about 8 s after the stop
/// (`about:blank` supersedes the in-flight load, WebKit reports the
/// cancellation, `enterOffline` re-arms `scheduleRetryTimer`'s 5 s delay), and
/// each subsequent attempt about 11 s after that, so 20 s holds two or three of
/// them. Anything above zero in this window is #6900.
let stopQuietSeconds: TimeInterval = 20
/// How long to wait for the view to open its first connection before issuing
/// the stop. The stop has to land on a load that is genuinely IN FLIGHT —
/// stopping an idle view proves nothing.
let stopInFlightWait: TimeInterval = 10

/// The Foundry dark background the view fills before drawing anything, from
/// `docs/design/UI/design-system/tokens.css` (`--trusty-content-bg: #201612`).
let backgroundRGB = (r: 0x20, g: 0x16, b: 0x12)
/// Brightest channel value still counted as black. #6838's bar, named once so
/// the whole-frame ratio and #6871's five edge samples cannot drift apart.
let nearBlackLevel = 8
/// How long `resize` mode waits for the console before growing the view. Only
/// the page-viewport assertion needs a live page; the frame assertions do not,
/// so a timeout here downgrades that one check rather than failing the run.
let resizeLiveWait: TimeInterval = 15

// MARK: - Visibility-recovery thresholds (#7112)

/// How long [`PageListener`] holds every request before answering. Two jobs: it
/// keeps the page from going live inside the 0.5 s every mode measures its
/// post-`startAnimation()` frame at, and it makes the view's `.suspended` state
/// last long enough to photograph.
let suspendResponseDelay: TimeInterval = 1.5
/// How long `suspend` mode waits for the first load to reach `.live`. Nothing to
/// assert until it does — both halves of #7112 are about a page already running.
let suspendLiveWait: TimeInterval = 20
/// How many times `suspend` mode re-issues `startAnimation()` over that live
/// page. `WallpaperAgent` was observed doing this every 20 s to 3.5 min.
let suspendReentrantCalls = 3
/// How many probes `suspend` mode's page answers `visible` before it flips. The
/// stretch this buys is what separates a view that recovers on real evidence
/// from one that reloads on every probe tick — the latter fails
/// [`suspendHealthySeconds`] below.
let suspendVisibleReads = 3
/// How long a HEALTHY page must go untouched. The view probes at 10 s, 20 s and
/// 30 s after `didFinish` and gets `visible` each time, so any reload inside
/// this window is a reload the page gave no reason for. Sits below the 40 s mark
/// where the fourth probe answers `hidden`.
let suspendHealthySeconds: TimeInterval = 35
/// How long to watch for the recovery reload after that. The fourth probe lands
/// at 40 s and answers `hidden`, so this window holds it with slack.
let suspendObservationSeconds: TimeInterval = 35
/// How long `suspend-cold` waits for its recovery. Its page answers `hidden` to
/// its first probe at 10 s and has no prior-visible history, so this measures the
/// view's bounded forced-recovery deadline: the escape hatch that stops a page
/// suspended inside its first probe interval from sitting in `.live` until the
/// hourly reload. Measured at 69 s on the reference host — a 10 s probe cadence
/// crossing a 60 s deadline — so 90 s holds it and 3600 s could never pass.
///
/// The view's OTHER ground for the same case — its own window not being
/// occluded — is not reachable here. AppKit reports no `.visible` for a window
/// parked off every display, and neither `alphaValue = 0` nor `0.01` on screen
/// changes that, so the only way to produce an unoccluded window is to put a
/// real one in front of the operator. See README.md, "Visibility recovery".
let suspendColdObservationSeconds: TimeInterval = 90
/// How long to sample the frame once the recovery reload is seen. The reload is
/// answered after [`suspendResponseDelay`], so the view sits in `.suspended` for
/// about that long and every sample in the window must carry drawn content.
let suspendSampleSeconds: TimeInterval = 3

// MARK: - Web-view rebuild thresholds (#7606)

/// Consecutive failed loads the view is expected to absorb before it rebuilds
/// its `WKWebView`. Mirrors `recreateAfterFailures` in `TrustyConsoleSaver.swift`
/// — the two must move together, and `one-failure` mode below is what catches a
/// view that rebuilds sooner.
let recreateAfterFailures = 3
/// How long `recreate` mode watches for that rebuild. Against a listener that
/// never answers, the view fails on its own 6 s watchdog and waits 8 s before
/// the next attempt, so the three failures land at roughly 6 s, 20 s and 34 s.
/// Sixty seconds holds the third with slack and is far short of the hourly
/// reload, which could produce a fresh load for a reason that is not this one.
let recreateObservationSeconds: TimeInterval = 60
/// How long `recreate` mode then watches for the load the rebuilt web view must
/// issue. A rebuild that replaces the instance and loads nothing is the same
/// black screen with an extra process behind it.
let recreateFreshLoadWait: TimeInterval = 10
/// How long `one-failure` mode waits for the retry that proves the FIRST failure
/// was counted and answered. Without it the mode would pass on a view that never
/// failed at all.
let oneFailureRetryWait: TimeInterval = 20
/// How much longer it then watches the instance. The second failure lands at
/// about 20 s and the rebuild at about 34 s, so this stays inside the window
/// where exactly one failure has been counted.
let oneFailureSettleSeconds: TimeInterval = 3

// MARK: - Occluded-window thresholds (#7846)

/// The view's `WindowVisibility` raw values, mirrored so the harness can force
/// one through KVC. They must move together with the enum in
/// `TrustyConsoleSaverView+Visibility.swift`; a mismatch reads as "ask AppKit" and the modes
/// below would then measure the harness's own offscreen window.
let visibilityVisible = 1
let visibilityHidden = 2
let visibilityUnknown = 3
/// The `@objc` setter the seam is reached through. Absent from any bundle built
/// before #7846, and `setValue(_:forKey:)` on a missing key raises an ObjC
/// exception Swift cannot catch — so it is probed, never assumed.
let visibilitySetterSelector = "setWindowVisibilityOverride:"

/// How long `occluded` and `occluded-failing` watch an occluded view. The fixed
/// view absorbs a timed-out load into a 60 s wait, so `occluded` sees exactly
/// the one attempt inside this window; the unfixed one fails at about 6 s and
/// retries every 8 s, so the same window holds four of its attempts and the
/// rebuild its third failure triggers at about 34 s.
let occludedObservationSeconds: TimeInterval = 45
/// Connection attempts `occluded-failing` demands. A connection the console
/// drops is a real failure whether or not anyone is looking at the screen, so
/// the retry backoff must keep running there — the fix suppresses the REBUILD,
/// not the retry. Well under what the unfixed and fixed bundles both produce.
let occludedFailingMinAttempts = 3
/// How long `occluded` then watches after the verdict flips to visible and the
/// occlusion notification is posted. The view ends its wait inside that
/// callback, so the attempt lands immediately and this is slack, not a budget.
let occludedRecoverySeconds: TimeInterval = 35
/// How long `visibility-unknown` requires the view to go without a rebuild.
/// Three re-asks at the 8 s fast retry interval put the fall-back at about 34 s
/// and the third COUNTED failure after it at about 62 s, where the unfixed view
/// — which never waits — reaches its third at about 34 s and rebuilds there.
let unknownQuietSeconds: TimeInterval = 45
/// How long it then watches for the rebuild that proves the fall-back fired
/// rather than the view waiting forever. Post-fix that lands at about 62 s, so
/// this window holds it with 18 s of slack.
let unknownFallbackSeconds: TimeInterval = 35
