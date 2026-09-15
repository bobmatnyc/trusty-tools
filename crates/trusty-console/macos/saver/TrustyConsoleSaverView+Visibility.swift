// TrustyConsoleSaverView — window visibility (#7846) and visibility recovery (#7112).
//
// Split from `TrustyConsoleSaver.swift` for the 500-SLOC cap (#7856). That
// file's header carries the Why/What/Test for the whole view.

import AppKit
import Foundation
import ScreenSaver
import WebKit
import os.log

extension TrustyConsoleSaverView {
    // MARK: - Window visibility (#7846)

    /// Whether this view is on screen at all, as AppKit sees it.
    ///
    /// #7846: distinct from `document.visibilityState`, which the view cannot
    /// use while a load is in flight — the web view is `isHidden` until
    /// `didFinish`, so WebKit reports its page hidden for every attempt,
    /// including the ones that are about to succeed.
    enum WindowVisibility: Int {
        /// The window reports itself unoccluded: the saver IS on screen, and a
        /// load that misses its deadline here missed it for a real reason.
        case visible = 1
        /// The window is occluded. WebKit throttles a backgrounded web view, so
        /// the deadline would measure the throttling and not the console.
        case hidden = 2
        /// Nothing has reported either way — no window yet. Treated as a wait,
        /// but a bounded one; see [`maxUnknownVisibilityProbes`].
        case unknown = 3
    }

    func currentWindowVisibility() -> WindowVisibility {
        if let forced = WindowVisibility(rawValue: windowVisibilityOverride) { return forced }
        guard let window else { return .unknown }
        return window.occlusionState.contains(.visible) ? .visible : .hidden
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
    /// Test: `PaintHarness/`'s `suspend` mode serves a page that reports
    ///   `visible` once and `hidden` afterwards, and asserts the view reloads.
    func armVisibilityProbe() {
        visibilityTimer?.invalidate()
        visibilityTimer = Timer.scheduledTimer(withTimeInterval: Self.visibilityProbeInterval,
                                               repeats: true) { [weak self] _ in
            self?.probeVisibility()
        }
    }

    func cancelVisibilityProbe() {
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
    /// Test: `PaintHarness/`'s `suspend` mode drives the transition ground,
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
    ///   `log show` carries them next to RunningBoard's own rows, re-probes the
    ///   moment the window is visible again instead of waiting out the rest of
    ///   the interval, and hands a load that waited out an occlusion its fresh
    ///   deadline (#7846).
    /// Test: manual — README.md, "Manual verification".
    func observeOcclusion() {
        guard occlusionObserver == nil else { return }
        // Seed from the window this view is in right now; the notification only
        // reports CHANGES, and a view that never sees one must not be left
        // guessing. No window yet means treated as on screen — see
        // [`windowIsOccluded`].
        windowIsOccluded = currentWindowVisibility() == .hidden
        occlusionObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeOcclusionStateNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self,
                  let changed = notification.object as? NSWindow,
                  changed === self.window else { return }
            // #7846: read through the same accessor the load deadline uses, so a
            // forced verdict cannot mean two different things in one view.
            let visible = self.currentWindowVisibility() != .hidden
            self.windowIsOccluded = !visible
            os_log("window occlusion changed — visible=%{public}@",
                   log: saverLog, type: .default, visible ? "true" : "false")
            guard visible else { return }
            self.probeVisibility()
            // See #7846: the load that was waiting for this is entitled to a
            // full deadline of on-screen time, not the remainder of one.
            self.resumeAfterWait()
        }
    }
}
