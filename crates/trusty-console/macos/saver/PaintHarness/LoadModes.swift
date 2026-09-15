// PaintHarness — the load-path modes: `resize`, `slow` and `stop`.
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

/// `resize` (#6871): the web view and the page's own viewport track a late host resize.
func assertResizeMode(_ view: ScreenSaverView) {
    // #6871: give the page a chance to come up FIRST, so the growth models a
    // host that hands over the real screen after the saver is already running —
    // the order the owner's ultrawide report happened in.
    let liveBy = Date().addingTimeInterval(resizeLiveWait)
    var live = false
    while Date() < liveBy && !live {
        RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        live = view.subviews.compactMap { $0 as? WKWebView }.first.map { !$0.isHidden } ?? false
    }
    note("console live before the resize: \(live)")

    note("resizing \(Int(initialSize.width))x\(Int(initialSize.height))"
        + " → \(Int(targetFrame.width))x\(Int(targetFrame.height))")
    window.setContentSize(targetFrame)
    RunLoop.current.run(until: Date().addingTimeInterval(1.0))

    guard let web = view.subviews.compactMap({ $0 as? WKWebView }).first else {
        failures.append("no web view to size — the view built none")
        return
    }
    note("after the resize: view.bounds=\(NSStringFromRect(view.bounds))"
        + " webView.frame=\(NSStringFromRect(web.frame))")
    // The issue's own closure condition: the web view owns the whole view.
    if web.frame != view.bounds {
        failures.append("web view does not track the bounds:"
            + " frame=\(NSStringFromRect(web.frame)) bounds=\(NSStringFromRect(view.bounds))")
    }

    // What the report was actually about — the PAGE's viewport, which the
    // bitmap cannot see (`edgeSamples` says why). Only a live page can answer.
    if web.isHidden {
        note("SKIP viewport check — the console never went live on this run")
    } else {
        var viewport: String?
        let answered = DispatchSemaphore(value: 0)
        web.evaluateJavaScript("[window.innerWidth, window.innerHeight].join('x')") { value, error in
            viewport = value as? String ?? "<error: \(error?.localizedDescription ?? "nil")>"
            answered.signal()
        }
        // The completion lands on the main queue, so the run loop has to turn.
        let answerBy = Date().addingTimeInterval(5)
        while answered.wait(timeout: .now()) == .timedOut && Date() < answerBy {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        let expected = "\(Int(view.bounds.width))x\(Int(view.bounds.height))"
        note("page viewport=\(viewport ?? "<timeout>") expected=\(expected)")
        if viewport != expected {
            failures.append("page viewport \(viewport ?? "<timeout>") != view bounds \(expected)")
        }
    }

    if let grownRep = capture(view), let samples = edgeSamples(of: grownRep) {
        note("edge samples: " + samples.map { "\($0.name)=(\($0.r),\($0.g),\($0.b))" }.joined(separator: " "))
        for sample in samples where max(sample.r, max(sample.g, sample.b)) <= nearBlackLevel {
            failures.append("frame is black at \(sample.name) after the resize:"
                + " (\(sample.r),\(sample.g),\(sample.b))")
        }
        if let grown = stats(of: grownRep) {
            note("frame after the resize: \(grown.summary)")
            if grown.nonBlackRatio < minNonBlackRatio {
                failures.append(String(format: "frame went black across the resize: nonBlack=%.4f", grown.nonBlackRatio))
            }
        }
    } else {
        failures.append("could not read the view's bitmap after the resize")
    }
}

/// `slow` (#6838): a load against a stalled endpoint times out and retries.
func assertSlowMode(_ view: ScreenSaverView) {
    guard let listener = silent else { return }
    note("watching the silent listener for \(Int(retryObservationSeconds))s of retry attempts")
    let deadline = Date().addingTimeInterval(retryObservationSeconds)
    while Date() < deadline {
        RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    let attempts = listener.accepted
    note("connection attempts: \(attempts)")
    if attempts < minSlowModeAttempts {
        failures.append("load never timed out: \(attempts) connection attempt(s), expected >= \(minSlowModeAttempts)")
    }
    // The frame must still be readable after the stall, not just at start.
    if let lateRep = capture(view), let lateFrame = stats(of: lateRep) {
        note("frame after the stall: \(lateFrame.summary)")
        if lateFrame.nonBlackRatio < minNonBlackRatio {
            failures.append(String(format: "frame went black during the stall: nonBlack=%.4f", lateFrame.nonBlackRatio))
        }
        if lateFrame.inkRatio < minInkRatio {
            failures.append(String(format: "fallback stopped drawing during the stall: ink=%.4f", lateFrame.inkRatio))
        }
    } else {
        failures.append("could not read the view's bitmap after the stall")
    }
}

/// `stop` (#6900): `stopAnimation()` returns inside budget and nothing loads afterwards.
func assertStopMode(_ view: ScreenSaverView) {
    guard let listener = silent else { return }

    /// Runs the run loop until `condition` holds or `seconds` elapse. The view's
    /// timers and WebKit's callbacks all land on the main run loop, so a plain
    /// sleep would stop the very machinery under test.
    func pump(_ seconds: TimeInterval, until condition: () -> Bool = { false }) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline && !condition() {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
    }

    /// One stop, measured. Returns the elapsed seconds and how many connections
    /// the listener saw in the quiet window that follows.
    ///
    /// `issue` is the caller's stop — a plain call for the in-flight case, a
    /// run-loop timer for the mid-render one — and is handed the clock so the
    /// budget covers `stopAnimation()` and nothing around it.
    func measureStop(_ label: String, issue: (@escaping () -> Void) -> Void) -> (elapsed: TimeInterval, leaked: Int) {
        let before = listener.accepted
        var elapsed: TimeInterval = -1
        issue {
            let started = Date()
            view.stopAnimation()
            elapsed = Date().timeIntervalSince(started)
        }
        pump(5, until: { elapsed >= 0 })
        note("\(label): stopAnimation() returned in "
            + String(format: "%.1f", elapsed * 1000) + "ms")
        if elapsed < 0 {
            failures.append("\(label): the stop never ran")
            return (elapsed, 0)
        }
        if elapsed > stopReturnBudget {
            failures.append(String(format: "%@: stopAnimation() took %.3fs, budget %.3fs",
                                   label, elapsed, stopReturnBudget))
        }
        note("\(label): watching for \(Int(stopQuietSeconds))s of traffic that must not come")
        pump(stopQuietSeconds)
        let leaked = listener.accepted - before
        note("\(label): connection attempts after the stop: \(leaked)")
        if leaked > 0 {
            failures.append("\(label): kept loading the console after the stop —"
                + " \(leaked) connection attempt(s), expected 0")
        }
        return (elapsed, leaked)
    }

    // --- the load must be in flight before the stop lands -------------------
    pump(stopInFlightWait, until: { listener.accepted > 0 })
    guard listener.accepted > 0 else {
        failures.append("the view never opened a connection — nothing to interrupt")
        return
    }
    note("load in flight after \(listener.accepted) connection attempt(s)")

    // --- 1: the stop lands on an in-flight load ----------------------------
    // This is #6900's reported shape: `loginwindow` stops the saver while its
    // WKWebView is mid-load.
    _ = measureStop("in-flight stop") { body in body() }

    // --- restart, then 2: the stop lands inside a render tick --------------
    // Restarting first proves the stopped state is not sticky (a wake that does
    // not unlock stops and re-arms the saver) and re-arms the loader that the
    // second stop has to interrupt.
    view.startAnimation()
    let afterRestart = listener.accepted
    pump(stopInFlightWait, until: { listener.accepted > afterRestart })
    if listener.accepted == afterRestart {
        failures.append("startAnimation() after a stop did not resume loading:"
            + " no new connection in \(Int(stopInFlightWait))s")
    }

    // The failure path #6900 asks for: the stop arrives while the view is
    // repainting. `animateOneFrame()` marks the view dirty and the stop is
    // issued in the same turn of the run loop, before that redraw is flushed —
    // the host's animation tick and `loginwindow`'s stop request landing
    // together.
    var midRenderFrame: PaintStats?
    _ = measureStop("mid-render stop") { body in
        Timer.scheduledTimer(withTimeInterval: 0.05, repeats: false) { _ in
            view.animateOneFrame()
            body()
            midRenderFrame = capture(view).flatMap(stats(of:))
        }
    }

    // #6838 must not regress on the way out: a view told to stop mid-frame
    // still paints the fallback rather than going black.
    if let painted = midRenderFrame {
        note("frame after the mid-render stop: \(painted.summary)")
        if painted.nonBlackRatio < minNonBlackRatio {
            failures.append(String(format: "frame went black across the mid-render stop: nonBlack=%.4f",
                                   painted.nonBlackRatio))
        }
    } else {
        failures.append("could not read the view's bitmap after the mid-render stop")
    }
}
