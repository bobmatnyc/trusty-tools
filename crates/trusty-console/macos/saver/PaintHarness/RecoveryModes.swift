// PaintHarness — the recovery modes: #7606 rebuild, #7112 suspend, #7846 occlusion.
//
// Split from `main.swift` for the 500-SLOC cap (#7856); that file's header
// carries the Why/What/Test for the whole harness.
//
// Each mode takes `view` as a parameter: a top-level `guard let` binding in
// `main.swift` is not visible from another file of the module.

import AppKit
import Foundation
import ScreenSaver
import WebKit

/// `recreate` and `one-failure` (#7606): three failed loads replace the web view; one does not.
func assertRebuildModes(_ view: ScreenSaverView) {
    guard let listener = silent else { return }
    let expectsRebuild = mode == "recreate"

    /// The view's timers and WebKit's callbacks all land on the main run loop,
    /// so a plain sleep would stop the machinery under test.
    func pump(_ seconds: TimeInterval, until condition: () -> Bool = { false }) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline && !condition() {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
    }

    func currentWebView() -> WKWebView? {
        view.subviews.compactMap { $0 as? WKWebView }.first
    }

    // The ORIGINAL instance, held strongly for the length of the run. Identity
    // is the assertion, and a released object's address can be handed to its
    // replacement — holding it makes `!==` mean what it reads as.
    guard let original = currentWebView() else {
        failures.append("the view built no web view to replace")
        return
    }
    note("original web view: \(ObjectIdentifier(original))")

    if expectsRebuild {
        // --- N consecutive failures must replace the instance ---------------
        // Every load against this listener times out, so the only variable is
        // how many the view takes before it stops reloading into the same
        // WebContent process and builds a new one. #7606's owner report is
        // three days of a view that never did.
        note("watching for the rebuild for \(Int(recreateObservationSeconds))s"
            + " (\(recreateAfterFailures) failures expected first)")
        var acceptedBeforeRebuild = listener.accepted
        var lowestInk = Double.greatestFiniteMagnitude
        let deadline = Date().addingTimeInterval(recreateObservationSeconds)
        while Date() < deadline && currentWebView() === original {
            // Sampled BEFORE the identity check, so this holds the connection
            // count from the last poll at which the instance was still the old
            // one — the count the rebuild's own load has to exceed.
            acceptedBeforeRebuild = listener.accepted
            if let rep = capture(view), let frame = stats(of: rep) {
                lowestInk = min(lowestInk, frame.inkRatio)
                if frame.nonBlackRatio < minNonBlackRatio {
                    failures.append(String(format: "frame went black while failing: nonBlack=%.4f",
                                           frame.nonBlackRatio))
                    break
                }
            }
            RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        }

        guard let rebuilt = currentWebView(), rebuilt !== original else {
            failures.append("the web view was never replaced:"
                + " \(listener.accepted) failed load(s) in \(Int(recreateObservationSeconds))s,"
                + " expected a rebuild after \(recreateAfterFailures)")
            return
        }
        note("web view replaced: \(ObjectIdentifier(original)) → \(ObjectIdentifier(rebuilt))"
            + " after \(acceptedBeforeRebuild) connection attempt(s)")
        if acceptedBeforeRebuild < recreateAfterFailures {
            failures.append("rebuilt too early: \(acceptedBeforeRebuild) attempt(s) before the"
                + " swap, expected at least \(recreateAfterFailures)")
        }

        // --- and the replacement must actually load -------------------------
        note("watching for the rebuilt view's own load for \(Int(recreateFreshLoadWait))s")
        pump(recreateFreshLoadWait, until: { listener.accepted > acceptedBeforeRebuild })
        note("connection attempts after the swap: \(listener.accepted) (was \(acceptedBeforeRebuild))")
        if listener.accepted <= acceptedBeforeRebuild {
            failures.append("the rebuilt web view never loaded:"
                + " no connection in \(Int(recreateFreshLoadWait))s after the swap")
        }

        // #6838 must not regress across the swap: the fallback keeps drawing
        // while the web view underneath it is replaced.
        note(String(format: "lowest ink while failing: %.4f", lowestInk))
        if lowestInk < minInkRatio, lowestInk != Double.greatestFiniteMagnitude {
            failures.append(String(format: "fallback stopped drawing while failing: ink=%.4f < %.4f",
                                   lowestInk, minInkRatio))
        }
    } else {
        // --- ONE failure must replace nothing --------------------------------
        // The counterpart assertion, and the one that fails a view that rebuilds
        // on every failure: a single timed-out load is an outage the retry
        // backoff already answers, and spending a WebContent process on it would
        // turn a brief console restart into process churn.
        note("waiting up to \(Int(oneFailureRetryWait))s for the retry that follows the first failure")
        pump(oneFailureRetryWait, until: { listener.accepted >= 2 })
        note("connection attempts: \(listener.accepted)")
        if listener.accepted < 2 {
            failures.append("the first load never failed and retried:"
                + " \(listener.accepted) connection attempt(s) in \(Int(oneFailureRetryWait))s,"
                + " so this run proves nothing about the rebuild threshold")
            return
        }
        pump(oneFailureSettleSeconds)
        if let now = currentWebView(), now !== original {
            failures.append("rebuilt the web view after a single failure —"
                + " \(listener.accepted) connection attempt(s), threshold is \(recreateAfterFailures)")
        } else {
            note("web view unchanged after one failure: \(ObjectIdentifier(original))")
        }
    }
}

/// `suspend` and `suspend-cold` (#7112): a re-entrant start reloads nothing and a hidden page recovers.
func assertSuspendModes(_ view: ScreenSaverView) {
    guard let listener = page else { return }
    let isCold = mode == "suspend-cold"

    /// The view's timers and WebKit's callbacks all land on the main run loop,
    /// so a plain sleep would stop the machinery under test.
    func pump(_ seconds: TimeInterval, until condition: () -> Bool = { false }) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline && !condition() {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
    }

    func pageIsOnScreen() -> Bool {
        view.subviews.compactMap { $0 as? WKWebView }.first.map { !$0.isHidden } ?? false
    }

    // --- the page must be live before either half of #7112 means anything ----
    pump(suspendLiveWait, until: pageIsOnScreen)
    guard pageIsOnScreen() else {
        failures.append("the page never went live in \(Int(suspendLiveWait))s —"
            + " \(listener.documentRequests) request(s) served")
        return
    }
    let afterLive = listener.documentRequests
    note("page live after \(afterLive) request(s)")

    // --- 1: a re-entrant startAnimation() must reload nothing ---------------
    // WallpaperAgent calls `startAnimation()` on an already-running view with no
    // `stopAnimation()` between. Before #7112 each call reset the state and
    // reloaded, which is the dashboard restarting from its first rotation frame.
    for _ in 0..<suspendReentrantCalls {
        view.startAnimation()
        pump(1)
    }
    let afterReentry = listener.documentRequests
    note("requests after \(suspendReentrantCalls) re-entrant startAnimation() call(s):"
        + " \(afterReentry) (was \(afterLive))")
    if afterReentry != afterLive {
        failures.append("re-entrant startAnimation() reloaded the page —"
            + " \(afterReentry - afterLive) extra request(s), expected 0")
    }
    if !pageIsOnScreen() {
        failures.append("re-entrant startAnimation() took the live page off screen")
    }

    // --- 2: a HEALTHY page must be left alone -------------------------------
    // `suspend`'s page answers three probes `visible` before it flips. A view
    // that reloaded on every probe tick would satisfy the recovery assertion
    // below just as well as a correct one, so this is what tells them apart:
    // for as long as the page reports itself visible, nothing may reload it.
    if !isCold {
        note("watching a healthy page for \(Int(suspendHealthySeconds))s of quiet")
        pump(suspendHealthySeconds)
        let afterHealthy = listener.documentRequests
        note("requests while the page reported visible: \(afterHealthy) (was \(afterReentry))")
        if afterHealthy != afterReentry {
            failures.append("reloaded a page that was reporting itself visible —"
                + " \(afterHealthy - afterReentry) request(s) before any reason to")
        }
        if !pageIsOnScreen() {
            failures.append("took a healthy page off screen")
        }
    }

    // --- 3: and a page that reports itself hidden must be recovered ---------
    // `suspend` gets there by transition. `suspend-cold`'s page is hidden from
    // its first answer, which has no history to reason from, so it measures the
    // bounded forced-recovery deadline — the escape hatch that stops a page
    // suspended inside its first probe interval from wedging the view in `.live`
    // until the hourly reload. See [`suspendColdObservationSeconds`] for the
    // ground this harness cannot reach.
    let recoveryWindow = isCold ? suspendColdObservationSeconds : suspendObservationSeconds
    note("watching for the recovery reload for \(Int(recoveryWindow))s")
    let beforeRecovery = listener.documentRequests
    pump(recoveryWindow, until: { listener.documentRequests > beforeRecovery })
    let afterRecovery = listener.documentRequests
    note("requests after the observation window: \(afterRecovery)")
    if afterRecovery <= beforeRecovery {
        failures.append("the view never recovered: no reload in"
            + " \(Int(recoveryWindow))s after the page reported itself hidden")
        return
    }

    // --- 4: and the screen must carry a real image while it recovers --------
    // #7112's symptom is that `draw(_:)` no-ops over a discarded layer, so the
    // frame during the recovery is the whole point. Only frames captured while
    // the web view is OFF screen are measured: those are the ones the view is
    // responsible for painting, and requiring at least one of them is also what
    // proves the view left `.live` rather than reloading underneath a live page.
    var lowestInk = Double.greatestFiniteMagnitude
    var offScreenSamples = 0
    let sampleUntil = Date().addingTimeInterval(suspendSampleSeconds)
    while Date() < sampleUntil {
        if !pageIsOnScreen(), let rep = capture(view), let frame = stats(of: rep) {
            lowestInk = min(lowestInk, frame.inkRatio)
            offScreenSamples += 1
            if frame.nonBlackRatio < minNonBlackRatio {
                failures.append(String(format: "frame went black during the recovery: nonBlack=%.4f",
                                       frame.nonBlackRatio))
                break
            }
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
    }
    note(String(format: "recovery window: %d off-screen sample(s), lowest ink=%.4f",
                offScreenSamples, lowestInk))
    if offScreenSamples == 0 {
        failures.append("the view reloaded without leaving the live state —"
            + " nothing repainted over the discarded layer")
    } else if lowestInk < minInkRatio {
        failures.append(String(format: "nothing drawn during the recovery: ink=%.4f < %.4f",
                               lowestInk, minInkRatio))
    }
}

/// `occluded`, `occluded-failing` and `visibility-unknown` (#7846): an occluded window waits, never fails.
func assertOcclusionModes(_ view: ScreenSaverView) {
    guard let listener = silent else { return }
    if !visibilitySeamPresent {
        // The mode cannot mean anything without the seam, and a bundle that
        // lacks it is a bundle from before the fix. Reported, then measured
        // anyway: the pre-fix behaviour under the harness's own occluded window
        // is exactly what the assertions below are written to catch.
        failures.append("no #7846 visibility seam in this bundle —"
            + " the verdict could not be forced, so this run is pre-fix behaviour")
    }

    /// The view's timers and WebKit's callbacks all land on the main run loop,
    /// so a plain sleep would stop the machinery under test.
    func pump(_ seconds: TimeInterval, until condition: () -> Bool = { false }) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline && !condition() {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
    }

    func currentWebView() -> WKWebView? {
        view.subviews.compactMap { $0 as? WKWebView }.first
    }

    // Held strongly for the length of the run: identity is the assertion, and a
    // released object's address can be handed to its replacement.
    guard let original = currentWebView() else {
        failures.append("the view built no web view")
        return
    }

    if mode == "visibility-unknown" {
        // --- an unanswerable window waits, but not forever -------------------
        // Nothing reports this view's window either way, so every timed-out load
        // is absorbed into a wait — until the probe budget runs out, after which
        // they are judged again. The REBUILD is what makes both halves visible
        // from outside: a view that never waited reaches three counted failures
        // inside the quiet window, and a view that waits forever never reaches
        // them at all. The connection cadence cannot tell those apart, because
        // the wait re-issues the attempt on the same fast interval the retry
        // backoff uses.
        note("watching an unknown-visibility view for \(Int(unknownQuietSeconds))s"
            + " — the first attempts must not be counted as failures")
        pump(unknownQuietSeconds)
        note("connection attempts while waiting: \(listener.accepted)")
        if let now = currentWebView(), now !== original {
            failures.append("counted an unanswerable window's timeouts as failures:"
                + " the web view was rebuilt inside \(Int(unknownQuietSeconds))s")
        }
        note("watching \(Int(unknownFallbackSeconds))s for the bounded fall-back")
        pump(unknownFallbackSeconds, until: { currentWebView() !== original })
        note("connection attempts after the fall-back window: \(listener.accepted)")
        if currentWebView() === original {
            failures.append("the view waited forever on an unknown verdict:"
                + " no rebuild in \(Int(unknownQuietSeconds + unknownFallbackSeconds))s,"
                + " so the bounded fall-back never fired")
        }
        return
    }

    // --- an occluded window is a wait, never a failure ----------------------
    // `occluded` stalls every load, so a view that respects the occlusion makes
    // exactly one attempt and keeps it. `occluded-failing` fails every load for
    // real, so the retries must continue — what must not happen there is the
    // rebuild, which would put a fresh WebContent process behind the same
    // occluded window and start the loop again.
    let stalls = mode == "occluded"
    note("watching an occluded view for \(Int(occludedObservationSeconds))s")
    var lowestInk = Double.greatestFiniteMagnitude
    let occludedUntil = Date().addingTimeInterval(occludedObservationSeconds)
    while Date() < occludedUntil {
        if let rep = capture(view), let frame = stats(of: rep) {
            lowestInk = min(lowestInk, frame.inkRatio)
            if frame.nonBlackRatio < minNonBlackRatio {
                failures.append(String(format: "frame went black while occluded: nonBlack=%.4f",
                                       frame.nonBlackRatio))
                break
            }
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
    }
    let occludedAttempts = listener.accepted
    note("connection attempts while occluded: \(occludedAttempts)")
    if stalls, occludedAttempts != 1 {
        failures.append("the occluded view gave up on a load WebKit was still throttling:"
            + " \(occludedAttempts) connection attempt(s) in \(Int(occludedObservationSeconds))s,"
            + " expected exactly 1")
    }
    if !stalls, occludedAttempts < occludedFailingMinAttempts {
        failures.append("the occluded view stopped retrying a console that was failing for real:"
            + " \(occludedAttempts) connection attempt(s), expected >= \(occludedFailingMinAttempts)")
    }
    if let now = currentWebView(), now !== original {
        failures.append("rebuilt the web view while occluded —"
            + " the replacement inherits the occlusion, which is the loop #7846 reports")
    }
    // #6838 must not regress across the wait: the fallback keeps drawing.
    note(String(format: "lowest ink while occluded: %.4f", lowestInk))
    if lowestInk < minInkRatio, lowestInk != Double.greatestFiniteMagnitude {
        failures.append(String(format: "fallback stopped drawing while occluded: ink=%.4f < %.4f",
                               lowestInk, minInkRatio))
    }

    // --- and a window that comes back gets the load judged after all --------
    guard stalls else { return }
    guard visibilitySeamPresent, forceVisibility(visibilityVisible, of: view) else {
        note("skipping the visible-again half: no seam to flip the verdict with")
        return
    }
    // The notification AppKit posts when a window stops being occluded. Posting
    // it by hand is the only way to reach that path here — the window is parked
    // off every display, so AppKit never posts it for real — and the view reads
    // the verdict back through the seam, not from the notification's payload.
    NotificationCenter.default.post(name: NSWindow.didChangeOcclusionStateNotification,
                                    object: window)
    note("verdict flipped to visible; watching \(Int(occludedRecoverySeconds))s"
        + " for the attempt that follows")
    pump(occludedRecoverySeconds, until: { listener.accepted > occludedAttempts })
    note("connection attempts after the window came back: \(listener.accepted)")
    if listener.accepted <= occludedAttempts {
        failures.append("the load was never judged once the window was visible:"
            + " still \(listener.accepted) connection attempt(s) after"
            + " \(Int(occludedRecoverySeconds))s — the deadline did not restart")
    }
}
