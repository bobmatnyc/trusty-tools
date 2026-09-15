// TrustyConsoleSaverView — loading, the load deadline, the #7606 rebuild, and the navigation callbacks.
//
// Split from `TrustyConsoleSaver.swift` for the 500-SLOC cap (#7856). That
// file's header carries the Why/What/Test for the whole view.

import AppKit
import Foundation
import ScreenSaver
import WebKit
import os.log

extension TrustyConsoleSaverView {
    // MARK: - Loading

    func loadConsole() {
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
    ///   view enters the offline state and the retry backoff takes over — unless
    ///   [`loadDeadlineIsDue`] says the window was not on screen to load into
    ///   (#7846). Called once per attempt, so the deferral bookkeeping resets
    ///   here and nowhere else.
    /// Test: `PaintHarness/`'s `slow` mode counts the retries this produces
    ///   against a listener that accepts and never answers; its `occluded` mode
    ///   asserts an occluded attempt produces none.
    private func startLoadWatchdog() {
        scheduleLoadDeadline(Self.loadDeadline)
    }

    /// Arms the one timer that owns both the deadline and the #7846 wait, so the
    /// two can never run against each other.
    ///
    /// The fired closure has three jobs: re-issue an attempt the wait outlived,
    /// keep waiting while the window is off screen, or judge the load the way
    /// this view always has.
    private func scheduleLoadDeadline(_ interval: TimeInterval) {
        loadTimer?.invalidate()
        loadTimer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.loadTimer = nil
            guard self.state != .live, self.state != .stopped, self.state != .preview else { return }
            // #7846: a wait tick whose attempt has already ended re-issues it.
            // Without this an occluded saver stops loading entirely, and a
            // console that came back while nobody was looking is never picked up.
            if self.waitingForWindow, self.loadStartedAt == nil {
                os_log("loading again after waiting for a visible window",
                       log: saverLog, type: .info)
                self.loadConsole()
                return
            }
            if let wait = self.windowWaitVerdict() {
                self.waitForWindow(wait.reason, recheckIn: wait.interval)
                return
            }
            if self.waitingForWindow, self.resumeAfterWait() { return }
            // #7606: giving up on an attempt means cancelling it. Left running,
            // it stays outstanding until the next `load` supersedes it, and the
            // -999 that comes back then is indistinguishable from a real one.
            self.abandonInFlightLoad()
            self.enterOffline("load did not finish within \(Int(Self.loadDeadline))s")
        }
    }

    /// Why: #7846. WebKit throttles a `WKWebView` in an occluded window, so an
    ///   attempt that runs there runs out of time because nothing is driving it,
    ///   not because the console is down. The owner's saver logged 3483 such
    ///   expiries against 23 completed loads over six hours while
    ///   `/ui/screensaver` answered in 0.6 ms throughout — each one an
    ///   [`enterOffline`], and every third one a #7606 rebuild whose fresh web
    ///   view inherited the same occlusion.
    /// What: answers whether this view may go on waiting for a window instead of
    ///   judging the load, and how long before it re-asks. An occluded window
    ///   waits at [`occludedRetryInterval`], which is deliberately slow — a
    ///   screen nobody can see earns no urgency. A window nobody can report on
    ///   waits at the fast retry cadence, but only [`maxUnknownVisibilityProbes`]
    ///   times: after that the count LATCHES at the bound and every later
    ///   expiry is judged the way it was before this fix, until some answer
    ///   either way clears it. A visible window never waits.
    /// Test: `PaintHarness/`'s `occluded` mode (no failure while occluded),
    ///   `visibility-unknown` (the bounded fall-back), and `slow` (the unchanged
    ///   visible path).
    private func windowWaitVerdict() -> (reason: String, interval: TimeInterval)? {
        switch currentWindowVisibility() {
        case .visible:
            unknownVisibilityProbes = 0
            return nil
        case .hidden:
            unknownVisibilityProbes = 0
            return ("occluded window", Self.occludedRetryInterval)
        case .unknown:
            guard unknownVisibilityProbes < Self.maxUnknownVisibilityProbes else { return nil }
            unknownVisibilityProbes += 1
            guard unknownVisibilityProbes < Self.maxUnknownVisibilityProbes else {
                // The fail-open's bound, logged once: the count stays at the
                // bound, so this line is not repeated until something answers.
                os_log("window visibility still unknown after %{public}@ probe(s) — judging the load on the ordinary deadline from here",
                       log: saverLog, type: .error, String(unknownVisibilityProbes))
                return nil
            }
            return ("window visibility unknown, probe \(unknownVisibilityProbes)"
                + " of \(Self.maxUnknownVisibilityProbes)", Self.fastRetryInterval)
        }
    }

    /// The wait itself. `waiting —` rather than `offline —`, because an operator
    /// reading `log show` has to be able to tell a throttled load from an outage
    /// (#7846), and the banner that says "offline" is never raised from here.
    private func waitForWindow(_ reason: String, recheckIn interval: TimeInterval) {
        let first = !waitingForWindow
        waitingForWindow = true
        if waitingForWindowSince == nil { waitingForWindowSince = Date() }
        let waited = waitingForWindowSince.map { Date().timeIntervalSince($0) } ?? 0
        // First of a run at `.default` so `log show` carries it after the fact;
        // the re-asks at `.info`, which is memory-only — a saver left occluded
        // overnight must not fill the persisted log with one line per re-ask.
        os_log("load deadline waiting — %{public}@ (%{public}@s so far)",
               log: saverLog, type: first ? .default : .info,
               reason, String(format: "%.0f", waited))
        scheduleLoadDeadline(interval)
    }

    /// Why: a load that spent its deadline throttled behind an occluded window
    ///   has not been given a chance to finish, so failing it the moment the
    ///   window returns would report an outage the console never had (#7846).
    /// What: ends the wait. An attempt still in flight gets one fresh FULL
    ///   deadline measured from the window coming back; an attempt the wait
    ///   outlived is simply re-issued, because the wait was the only thing
    ///   holding it. Reached from the occlusion notification, which is what
    ///   normally observes the change, and from the re-ask above when no
    ///   notification arrives.
    /// Test: `PaintHarness/`'s `occluded` mode flips the visibility seam,
    ///   posts the occlusion notification AppKit would, and asserts the attempt
    ///   that follows.
    @discardableResult
    func resumeAfterWait() -> Bool {
        guard waitingForWindow, state != .live, state != .stopped, state != .preview else { return false }
        let waited = waitingForWindowSince.map { Date().timeIntervalSince($0) } ?? 0
        waitingForWindow = false
        waitingForWindowSince = nil
        unknownVisibilityProbes = 0
        os_log("window visible again after %{public}@s of waiting — %{public}@",
               log: saverLog, type: .default, String(format: "%.0f", waited),
               loadStartedAt == nil ? "loading" : "restarting the load deadline")
        if loadStartedAt == nil {
            loadConsole()
        } else {
            startLoadWatchdog()
        }
        return true
    }

    /// Why: a request that times out while the window is off screen says the
    ///   load ran out of time, which is what an occluded web view does — WebKit
    ///   throttles the page while the network process goes on running the 5 s
    ///   request timer. It is not evidence about the console, and #7846 is what
    ///   happens when the view treats it as evidence anyway.
    /// What: absorbs `NSURLErrorTimedOut` into the wait when the window is not
    ///   on screen, ending the attempt and arming the re-ask that will re-issue
    ///   it. Every other error — refused, connection lost, a dead WebContent
    ///   process — is untouched and still reaches [`enterOffline`], because those
    ///   say something about the console that occlusion does not explain.
    /// Test: `PaintHarness/`'s `occluded` mode, whose stalling endpoint
    ///   produces exactly this error, against `occluded-failing`, whose hang-up
    ///   endpoint does not.
    private func absorbTimeoutWhileOccluded(_ nsError: NSError) -> Bool {
        guard nsError.domain == NSURLErrorDomain, nsError.code == NSURLErrorTimedOut,
              state != .stopped, state != .preview,
              let wait = windowWaitVerdict() else { return false }
        loadStartedAt = nil
        waitForWindow(wait.reason + ", the load timed out", recheckIn: wait.interval)
        return true
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
    /// Test: `PaintHarness/`'s `recreate` mode counts the failures the view
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

    func scheduleReloadTimer() {
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
    /// Test: `PaintHarness/`'s `slow` mode; `stop` mode for the `.stopped`
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
    /// Test: `PaintHarness/`'s `recreate` mode asserts the instance is
    ///   replaced after three failures and a fresh load follows; its `one-failure`
    ///   mode asserts a single failure replaces nothing.
    @discardableResult
    func noteLoadFailure(_ reason: String) -> Bool {
        guard state != .stopped, state != .preview else { return false }
        // #7846: a replacement web view is built into the same occluded window
        // and is throttled the same way, so a failure counted here can only feed
        // a rebuild loop — 1146 of them in the owner's six-hour log. Retries
        // still run, because a console that comes back must be picked up whether
        // or not anyone is looking at the screen.
        guard currentWindowVisibility() != .hidden else {
            os_log("failure not counted toward the rebuild — occluded window (%{public}@)",
                   log: saverLog, type: .info, reason)
            return false
        }
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
    /// Test: `PaintHarness/`'s `recreate` mode.
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
        // #7846: this attempt is over, so its deferral bookkeeping is too.
        waitingForWindow = false
        waitingForWindowSince = nil
        unknownVisibilityProbes = 0
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

    /// Why: #7606 asks that a cancellation this view provoked in
    ///   [`abandonInFlightLoad`] is not counted as a network failure — the
    ///   attempt replacing it is already accounted for, and counting it turned
    ///   one failure into two and halved the effective retry interval. The check
    ///   sat only on the provisional callback, so a cancellation that landed
    ///   after the navigation had COMMITTED reached `didFail` and was counted
    ///   anyway; `didFail code=-999` is what the owner's #7846 log is full of.
    /// What: shared by both failure callbacks. Clears the flag either way, since
    ///   the attempt it belonged to has ended.
    /// Test: `PaintHarness/`'s `recreate` and `one-failure` modes count the
    ///   failures the view reaches before it rebuilds.
    private func isOwnCancellation(_ nsError: NSError) -> Bool {
        defer { expectingCancellation = false }
        guard expectingCancellation,
              nsError.domain == NSURLErrorDomain,
              nsError.code == NSURLErrorCancelled else { return false }
        os_log("ignored own cancellation — %{public}@", log: saverLog, type: .info,
               Self.describe(nsError))
        return true
    }

    public func webView(_ webView: WKWebView,
                        didFailProvisionalNavigation navigation: WKNavigation!,
                        withError error: Error) {
        let nsError = error as NSError
        if isOwnCancellation(nsError) { return }
        // See #7846.
        if absorbTimeoutWhileOccluded(nsError) { return }
        enterOffline("didFailProvisionalNavigation " + Self.describe(nsError))
    }

    public func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        let nsError = error as NSError
        // See #7846.
        if isOwnCancellation(nsError) { return }
        if absorbTimeoutWhileOccluded(nsError) { return }
        enterOffline("didFail " + Self.describe(nsError))
    }

    public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        enterOffline("WebContent process terminated")
    }

    static func describe(_ error: NSError) -> String {
        "domain=\(error.domain) code=\(error.code) desc=\(error.localizedDescription)"
    }
}
