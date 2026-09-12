// PaintHarness — what the saver actually PAINTS, as an exit code (#6838, #6839).
//
// Why: `LoadHarness.swift` proves the happy path — bundle loads, principal class
//   resolves, the console page finishes. It says nothing about the three states
//   an operator actually complained about: the daemon down, the daemon bound but
//   not yet answering, and the System Settings gallery tile. All three are
//   "whatever the view paints when there is no live page", and all three were
//   reported as a black screen. Nothing measured them, because measuring them
//   means reading pixels, not navigation callbacks.
// What: nine modes, each instantiating the bundle's principal class offscreen
//   and reading its rendered bitmap through
//   `bitmapImageRepForCachingDisplay` / `cacheDisplay`:
//     offline — points the view at a closed port; asserts the frame is not black
//               and carries real content from the moment the view exists.
//     slow    — points the view at a listener that ACCEPTS and never answers (a
//               daemon that has bound its socket mid-restart); asserts the same,
//               then counts connection attempts to prove the load times out and
//               retries instead of hanging.
//     preview — instantiates with `isPreview: true`; asserts the bundled static
//               asset is what draws, and that no web view is built for a tile.
//     resize  — #6871: constructs the view SMALL, animates it, then grows it to
//               the target frame the way a host that learns the screen late
//               would; asserts the web view tracks `bounds`, that the page's own
//               viewport matches, and that the frame is not black at its edges.
//     stop    — #6900: points the view at the same never-answering listener so a
//               load is genuinely in flight, then issues `stopAnimation()` the
//               way `loginwindow` does; asserts the call returns inside the
//               issue's 500 ms bar and that the listener sees no further
//               connection, twice — once on an in-flight load and once from
//               inside a render tick.
//     suspend — #7112: the only modes with an endpoint that ANSWERS, so the view
//     suspend-  reaches `.live`. Both assert a re-entrant `startAnimation()` —
//     cold      what `WallpaperAgent` does every 20 s to 3.5 min — reloads
//               nothing, and that the recovery repaints rather than leaving a
//               blank frame. `suspend` serves a page that answers three probes
//               `visible` before flipping, so it also asserts a HEALTHY page is
//               never reloaded — which is what fails a view that reloads on
//               every probe tick. `suspend-cold` serves one hidden from its
//               first answer, and measures the bounded forced-recovery deadline.
//     recreate  #7606: the never-answering listener again, so every load fails
//               the same way and the failure COUNT is the variable. Asserts the
//               view stops reloading into the same WebContent process and
//               replaces the `WKWebView` instance after three consecutive
//               failures, and that the replacement loads.
//     one-      the same endpoint, watched only as far as the first failure and
//     failure   its retry. Asserts the instance is NOT replaced there — the
//               assertion that fails a view which spends a process on every
//               brief console restart.
//   Every mode takes the frame it runs at — `--frame WxH`, default 1280x800 —
//   so the ultrawide geometry #6871 was reported on is reachable.
// Test: it IS the test. README.md, "Paint harness", has the invocations;
//   `scripts/build-console-saver.sh` builds the bundle it consumes.
//
// Like `LoadHarness.swift` it runs UNSANDBOXED, so it proves the drawing code,
// not the sandboxed `legacyScreenSaver.appex` host. The in-host run stays manual.

import AppKit
import Foundation
import Network
import ScreenSaver
import WebKit

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

// MARK: - Arguments

func note(_ message: String) {
    FileHandle.standardError.write("PAINT: \(message)\n".data(using: .utf8)!)
}

/// `WxH` → an `NSSize`. 0 is ACCEPTED: a 0x0 start frame is the degenerate case
/// #6871 exists to exercise, not a typo to reject.
func parseSize(_ text: String) -> NSSize? {
    let parts = text.lowercased().split(separator: "x", maxSplits: 1)
    guard parts.count == 2,
          let width = Double(parts[0]), let height = Double(parts[1]),
          width >= 0, height >= 0, width <= 32768, height <= 32768 else { return nil }
    return NSSize(width: width, height: height)
}

/// Unchanged from the harness's first cut, so an invocation with no `--frame`
/// still measures what the #6838/#6839 ink table was measured at.
let defaultFrameSize = NSSize(width: 1280, height: 800)
/// What `resize` starts at: the rough size of a System Settings preview, i.e. a
/// plausible frame for a host that has not yet decided which screen this is.
let defaultStartSize = NSSize(width: 320, height: 200)

var positional: [String] = []
var frameSize: NSSize?
var startSize: NSSize?

var pending = Array(CommandLine.arguments.dropFirst())
while let arg = pending.first {
    pending.removeFirst()
    switch arg {
    case "--frame", "--start":
        guard let value = pending.first, let size = parseSize(value) else {
            note("\(arg) needs a WxH value, e.g. \(arg) 3440x1440")
            exit(64)
        }
        pending.removeFirst()
        if arg == "--frame" { frameSize = size } else { startSize = size }
    default:
        positional.append(arg)
    }
}

// Env is the fallback, not an override: an explicit flag wins.
if frameSize == nil, let env = ProcessInfo.processInfo.environment["SAVER_HARNESS_FRAME"] {
    guard let size = parseSize(env) else {
        note("SAVER_HARNESS_FRAME=\(env) is not a WxH size")
        exit(64)
    }
    frameSize = size
}

let targetFrame = frameSize ?? defaultFrameSize
let resizeStart = startSize ?? defaultStartSize

let mode = positional.count > 0 ? positional[0] : ""
let bundlePath = positional.count > 1
    ? positional[1]
    : NSHomeDirectory() + "/Library/Screen Savers/TrustyConsole.saver"

guard ["offline", "slow", "preview", "resize", "stop", "suspend", "suspend-cold",
       "recreate", "one-failure"].contains(mode) else {
    note("usage: paintharness <offline|slow|preview|resize|stop|suspend|suspend-cold"
        + "|recreate|one-failure> [bundlePath] [--frame WxH] [--start WxH]")
    note("  --frame  the frame to run at (default 1280x800; env SAVER_HARNESS_FRAME)")
    note("  --start  resize mode only: the frame to construct at (default 320x200)")
    exit(64)
}

// MARK: - Defaults override (restored before exit)

let defaultsDomain = "com.trusty.console.saver"
let portKey = "ConsolePort"
let pathKey = "ConsolePath"
/// The console's default port, mirrored from `SaverConfig.defaultPort` — the
/// port `resize` mode looks for a live dashboard on.
let SaverDefaultPort = 7788
let saverDefaults = ScreenSaverDefaults(forModuleWithName: defaultsDomain)
let priorPort = saverDefaults?.object(forKey: portKey)
let priorPath = saverDefaults?.object(forKey: pathKey)

func restoreDefaults() {
    guard let saverDefaults else { return }
    if let priorPort { saverDefaults.set(priorPort, forKey: portKey) } else { saverDefaults.removeObject(forKey: portKey) }
    if let priorPath { saverDefaults.set(priorPath, forKey: pathKey) } else { saverDefaults.removeObject(forKey: pathKey) }
    saverDefaults.synchronize()
}

func finish(_ code: Int32) -> Never {
    restoreDefaults()
    exit(code)
}

func pointView(atPort port: Int) {
    guard let saverDefaults else {
        note("ScreenSaverDefaults unavailable — cannot steer the view at a test port")
        finish(6)
    }
    saverDefaults.set(port, forKey: portKey)
    saverDefaults.set("/ui/screensaver", forKey: pathKey)
    saverDefaults.synchronize()
    note("pointing the view at 127.0.0.1:\(port)/ui/screensaver")
}

// MARK: - Test endpoints

/// A port nothing is listening on: bind an ephemeral one, read it, release it.
/// Racy in principle, unreachable in practice on a loopback-only test host, and
/// far safer than hardcoding a number some other service may hold.
func closedPort() -> Int {
    let listener: NWListener
    do {
        listener = try NWListener(using: .tcp, on: .any)
    } catch {
        note("could not bind an ephemeral port: \(error)")
        finish(6)
    }
    let ready = DispatchSemaphore(value: 0)
    listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
    listener.newConnectionHandler = { $0.cancel() }
    listener.start(queue: .global())
    _ = ready.wait(timeout: .now() + 5)
    let port = Int(listener.port?.rawValue ?? 0)
    listener.cancel()
    guard port > 0 else {
        note("ephemeral listener never reported a port")
        finish(6)
    }
    return port
}

/// A listener that completes the TCP handshake and then says nothing — the
/// shape of a daemon that has bound its socket during a restart but cannot yet
/// answer an HTTP request. Counts every connection it accepts, which is how the
/// harness sees the view give up and retry.
final class SilentListener {
    private let listener: NWListener
    private let lock = NSLock()
    private var connections: [NWConnection] = []
    private var count = 0

    /// Defaulted rather than assigned once at the end, because the connection
    /// handler below captures `self` and Swift will not allow that until every
    /// stored property holds a value.
    private(set) var port = 0

    init?() {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { return }
            self.lock.lock()
            self.count += 1
            // Held so ARC does not release the connection and close the socket,
            // which would look to the client like a refusal rather than a stall.
            self.connections.append(connection)
            self.lock.unlock()
            connection.start(queue: .global())
        }
        listener.start(queue: .global())
        guard ready.wait(timeout: .now() + 5) == .success,
              let bound = listener.port?.rawValue, bound > 0 else {
            listener.cancel()
            return nil
        }
        port = Int(bound)
    }

    var accepted: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func stop() {
        lock.lock()
        connections.forEach { $0.cancel() }
        connections.removeAll()
        lock.unlock()
        listener.cancel()
    }
}

/// A listener that actually ANSWERS, so the view reaches `.live` — the state
/// both halves of #7112 happen in, and the one `SilentListener` cannot produce.
///
/// The page it serves reports `document.visibilityState === 'visible'` to its
/// first `visibleReads` reads and `'hidden'` to every read after them. That is
/// the transition WebKit performs when RunningBoard marks the WebContent process
/// NotVisible and the layer trees are frozen, reproduced without needing the OS
/// to do it: the saver's probe is the only reader, so the flip is deterministic
/// rather than timed, and counting reads is what lets the harness demand the
/// view leave a HEALTHY page alone for a stretch first.
///
/// `visibleReads: 0` serves a page that is hidden from its very first answer —
/// the case with no visible-then-hidden history to reason from, which the view
/// must still recover because its own window is not occluded.
///
/// Every response is held for [`suspendResponseDelay`] — see that constant.
/// Only requests for the console path are counted, so a favicon or any other
/// incidental fetch cannot be mistaken for a reload.
final class PageListener {
    private let listener: NWListener
    private let lock = NSLock()
    private var count = 0

    private(set) var port = 0

    /// Requests the view has made for the console path.
    var documentRequests: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    private let body: String

    static func page(visibleReads: Int) -> String {
        """
        <!doctype html><html><head><meta charset="utf-8"><title>suspend harness</title>
        <style>html,body{margin:0;height:100%;background:#201612;color:#f0e7d8;\
        font:48px monospace;display:flex;align-items:center;justify-content:center}</style>
        </head><body><div>suspend harness</div><script>
        var probes = 0;
        Object.defineProperty(document, 'visibilityState', {
          configurable: true,
          get: function () {
            probes += 1;
            return probes <= \(visibleReads) ? 'visible' : 'hidden';
          }
        });
        </script></body></html>
        """
    }

    init?(visibleReads: Int) {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
        self.body = Self.page(visibleReads: visibleReads)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] connection in
            connection.start(queue: .global())
            self?.serve(connection)
        }
        listener.start(queue: .global())
        guard ready.wait(timeout: .now() + 5) == .success,
              let bound = listener.port?.rawValue, bound > 0 else {
            listener.cancel()
            return nil
        }
        port = Int(bound)
    }

    /// One request, one response, one close. A loopback GET arrives in a single
    /// segment, so this reads once rather than accumulating a full header block.
    private func serve(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, _, _ in
            guard let self else { return }
            let request = data.flatMap { String(data: $0, encoding: .utf8) } ?? ""
            guard request.contains(" /ui/screensaver") else {
                connection.cancel()
                return
            }
            self.lock.lock()
            self.count += 1
            self.lock.unlock()

            let payload = Data(self.body.utf8)
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"
                + "Content-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
            DispatchQueue.global().asyncAfter(deadline: .now() + suspendResponseDelay) {
                connection.send(content: Data(head.utf8) + payload,
                                completion: .contentProcessed { _ in connection.cancel() })
            }
        }
    }

    func stop() { listener.cancel() }
}

// MARK: - Pixel measurement

struct PaintStats {
    let width: Int
    let height: Int
    /// Pixels brighter than near-black, as a fraction of the frame.
    let nonBlackRatio: Double
    /// Pixels differing from the flat Foundry background, as a fraction of the
    /// frame — the frame's drawn content.
    let inkRatio: Double

    var summary: String {
        String(format: "%dx%d nonBlack=%.4f ink=%.4f", width, height, nonBlackRatio, inkRatio)
    }
}

/// Read the view's own rendering, not a screenshot of the window: `cacheDisplay`
/// runs `draw(_:)` into the rep synchronously, so this captures the drawing code
/// and nothing about the compositor.
///
/// Split from [`stats`] so the first-paint clock can be stamped the moment the
/// frame EXISTS. Counting a million pixels is the harness's own cost and has no
/// business inside a latency budget the view is being judged against.
func capture(_ view: NSView) -> NSBitmapImageRep? {
    guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return nil }
    view.cacheDisplay(in: view.bounds, to: rep)
    return rep
}

/// Decodes a captured rep into a tightly-packed sRGB RGBA buffer. Shared by the
/// whole-frame ratios and #6871's five edge samples so both read the same
/// pixels through the same colour space.
func pixels(of rep: NSBitmapImageRep) -> (width: Int, height: Int, buffer: [UInt8])? {
    guard let cgImage = rep.cgImage else { return nil }

    let width = cgImage.width
    let height = cgImage.height
    guard width > 0, height > 0 else { return nil }

    var buffer = [UInt8](repeating: 0, count: width * height * 4)
    let drew: Bool = buffer.withUnsafeMutableBytes { raw -> Bool in
        guard let base = raw.baseAddress,
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                data: base,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: width * 4,
                space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return false }
        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
        return true
    }
    guard drew else { return nil }
    return (width, height, buffer)
}

func stats(of rep: NSBitmapImageRep) -> PaintStats? {
    guard let (width, height, buffer) = pixels(of: rep) else { return nil }

    var nonBlack = 0
    var ink = 0
    var index = 0
    let total = width * height
    while index < total {
        let offset = index * 4
        let r = Int(buffer[offset])
        let g = Int(buffer[offset + 1])
        let b = Int(buffer[offset + 2])
        if max(r, max(g, b)) > nearBlackLevel { nonBlack += 1 }
        let distance = abs(r - backgroundRGB.r) + abs(g - backgroundRGB.g) + abs(b - backgroundRGB.b)
        if distance > 24 { ink += 1 }
        index += 1
    }

    return PaintStats(
        width: width,
        height: height,
        nonBlackRatio: Double(nonBlack) / Double(total),
        inkRatio: Double(ink) / Double(total))
}

/// #6871: one sample inside each corner plus the centre — the five points a web
/// view that failed to grow would leave unpainted along an edge.
///
/// This is the "never black" bar (#6838) restated at the target frame, and that
/// is ALL it is. `cacheDisplay` reads the view's own `draw(_:)`, and the live
/// page's background is the same `#201612` the view fills with, so these samples
/// cannot tell a correctly sized page from a letterboxed one. The frame equality
/// and the page-viewport check are what prove the fit.
func edgeSamples(of rep: NSBitmapImageRep) -> [(name: String, r: Int, g: Int, b: Int)]? {
    guard let (width, height, buffer) = pixels(of: rep) else { return nil }
    let inset = 8
    guard width > inset * 2, height > inset * 2 else { return nil }
    let points: [(String, Int, Int)] = [
        ("top-left", inset, inset),
        ("top-right", width - 1 - inset, inset),
        ("bottom-left", inset, height - 1 - inset),
        ("bottom-right", width - 1 - inset, height - 1 - inset),
        ("centre", width / 2, height / 2),
    ]
    return points.map { name, x, y in
        let offset = (y * width + x) * 4
        return (name, Int(buffer[offset]), Int(buffer[offset + 1]), Int(buffer[offset + 2]))
    }
}

// MARK: - Bundle load

guard let bundle = Bundle(path: bundlePath) else {
    note("Bundle(path:) returned nil for \(bundlePath)")
    finish(2)
}
note("mode=\(mode) bundle=\(bundlePath) loaded=\(bundle.load())")

guard let principal = bundle.principalClass, let saverClass = principal as? ScreenSaverView.Type else {
    note("NSPrincipalClass did not resolve to a ScreenSaverView subclass")
    finish(3)
}

// MARK: - Endpoint setup

var silent: SilentListener?
var page: PageListener?
switch mode {
case "offline":
    pointView(atPort: closedPort())
case "suspend", "suspend-cold":
    // #7112 happens to a page that is already live, so these modes need an
    // endpoint that answers rather than one that stalls. `suspend-cold` serves a
    // page that is hidden from its first answer; `suspend` one that is healthy
    // for three probes first.
    guard let listener = PageListener(visibleReads: mode == "suspend" ? suspendVisibleReads : 0) else {
        note("could not start the page listener")
        finish(6)
    }
    page = listener
    pointView(atPort: listener.port)
case "slow", "stop", "recreate", "one-failure":
    // #6900 wants the same endpoint `slow` uses: a load that is in flight and
    // stays there is the state the stop has to interrupt, and every connection
    // the view opens is counted. #7606's two modes want it for a third reason —
    // it is the only endpoint that makes every load fail the same way, so the
    // failure COUNT is the variable under test.
    guard let listener = SilentListener() else {
        note("could not start the silent listener")
        finish(6)
    }
    silent = listener
    pointView(atPort: listener.port)
case "resize":
    // #6871 is a LIVE-page defect, so this mode points at the real console
    // rather than a stand-in. It still runs without one — only the viewport
    // assertion needs the page.
    let port = ProcessInfo.processInfo.environment["SAVER_HARNESS_PORT"].flatMap(Int.init)
        ?? SaverDefaultPort
    pointView(atPort: port)
default:
    break // preview never touches the network
}

// MARK: - Instantiate offscreen

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

// #6871: `resize` deliberately constructs the view SMALL and grows it later, so
// it is the one mode whose initial frame is not the frame under test.
let initialSize = mode == "resize" ? resizeStart : targetFrame
let frame = NSRect(origin: .zero, size: initialSize)
let isPreview = mode == "preview"
/// A 0x0 frame has no bitmap to read — `resize --start 0x0` asks for exactly
/// that, and its assertions all land after the growth.
let initialFrameIsMeasurable = initialSize.width > 0 && initialSize.height > 0

var failures: [String] = []

guard let view = saverClass.init(frame: frame, isPreview: isPreview) else {
    note("init(frame:isPreview:) returned nil")
    finish(5)
}
let readyAt = Date()

let window = NSWindow(contentRect: frame, styleMask: [.borderless], backing: .buffered, defer: false)
window.contentView = view
window.orderFrontRegardless()
window.setFrameOrigin(NSPoint(x: -5000, y: -5000)) // offscreen: do not disturb the operator

note("constructed at \(Int(initialSize.width))x\(Int(initialSize.height)); view.bounds=\(NSStringFromRect(view.bounds))")

// No run loop first: `cacheDisplay` drives `draw(_:)` synchronously, so this is
// the earliest frame the view can possibly produce.
if initialFrameIsMeasurable {
    guard let firstRep = capture(view) else {
        note("FAIL — could not read the view's bitmap")
        silent?.stop()
        page?.stop()
        finish(8)
    }
    let firstPaintElapsed = Date().timeIntervalSince(readyAt)
    guard let firstFrame = stats(of: firstRep) else {
        note("FAIL — could not decode the captured bitmap")
        silent?.stop()
        page?.stop()
        finish(8)
    }
    note("first frame at \(String(format: "%.2f", firstPaintElapsed))s after init returned: \(firstFrame.summary)")

    if firstPaintElapsed > firstPaintDeadline {
        failures.append(String(format: "first frame took %.2fs, budget %.2fs", firstPaintElapsed, firstPaintDeadline))
    }
    if firstFrame.nonBlackRatio < minNonBlackRatio {
        failures.append(String(format: "frame is black: nonBlack=%.4f < %.4f", firstFrame.nonBlackRatio, minNonBlackRatio))
    }
    if firstFrame.inkRatio < minInkRatio {
        failures.append(String(format: "no static fallback drawn: ink=%.4f < %.4f", firstFrame.inkRatio, minInkRatio))
    }
} else {
    note("start frame is 0x0 — no bitmap to read before the resize")
}

// Now start it, and confirm the frame survives the load attempt. No time budget
// here — see `firstPaintDeadline` for why `startAnimation()` is not the view's
// latency to answer for.
view.startAnimation()
RunLoop.current.run(until: Date().addingTimeInterval(0.5))
if !initialFrameIsMeasurable {
    note("skipping the post-startAnimation capture: the view is still 0x0")
} else if let animRep = capture(view), let animFrame = stats(of: animRep) {
    note("frame after startAnimation: \(animFrame.summary)")
    if animFrame.nonBlackRatio < minNonBlackRatio {
        failures.append(String(format: "frame went black once animating: nonBlack=%.4f", animFrame.nonBlackRatio))
    }
    if animFrame.inkRatio < minInkRatio {
        failures.append(String(format: "fallback stopped drawing once animating: ink=%.4f", animFrame.inkRatio))
    }
} else {
    failures.append("could not read the view's bitmap after startAnimation")
}

// MARK: - Per-mode assertions

switch mode {
case "preview":
    // A tile must not cost a WebContent XPC child; the asset is the whole point.
    if view.subviews.contains(where: { $0 is WKWebView }) {
        failures.append("preview built a WKWebView")
    }

case "resize":
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
        break
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

case "slow":
    guard let listener = silent else { break }
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

case "recreate", "one-failure":
    guard let listener = silent else { break }
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
        break
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
            break
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
            break
        }
        pump(oneFailureSettleSeconds)
        if let now = currentWebView(), now !== original {
            failures.append("rebuilt the web view after a single failure —"
                + " \(listener.accepted) connection attempt(s), threshold is \(recreateAfterFailures)")
        } else {
            note("web view unchanged after one failure: \(ObjectIdentifier(original))")
        }
    }

case "stop":
    guard let listener = silent else { break }

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
        break
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

case "suspend", "suspend-cold":
    guard let listener = page else { break }
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
        break
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
        break
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

default:
    break
}

view.stopAnimation()
silent?.stop()
page?.stop()

if failures.isEmpty {
    note("PASS — \(mode)")
    finish(0)
}
for failure in failures { note("FAIL — \(failure)") }
finish(9)
