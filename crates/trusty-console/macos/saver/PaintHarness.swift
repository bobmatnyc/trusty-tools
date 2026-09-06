// PaintHarness — what the saver actually PAINTS, as an exit code (#6838, #6839).
//
// Why: `LoadHarness.swift` proves the happy path — bundle loads, principal class
//   resolves, the console page finishes. It says nothing about the three states
//   an operator actually complained about: the daemon down, the daemon bound but
//   not yet answering, and the System Settings gallery tile. All three are
//   "whatever the view paints when there is no live page", and all three were
//   reported as a black screen. Nothing measured them, because measuring them
//   means reading pixels, not navigation callbacks.
// What: four modes, each instantiating the bundle's principal class offscreen
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
/// view attempts at roughly 0 s, 11 s, 22 s and 33 s (a 5 s request timeout with
/// a 1 s watchdog grace, plus a 5 s retry delay), so this window holds four
/// attempts and demands three.
let retryObservationSeconds: TimeInterval = 34
/// Connection attempts the `slow` mode expects inside that window: the initial
/// load plus at least two retries. Without a request timeout the view issues
/// one connection and waits out `URLRequest`'s 60 s default, so this is the
/// assertion that fails on the unfixed bundle.
let minSlowModeAttempts = 3

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

guard ["offline", "slow", "preview", "resize"].contains(mode) else {
    note("usage: paintharness <offline|slow|preview|resize> [bundlePath]"
        + " [--frame WxH] [--start WxH]")
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
switch mode {
case "offline":
    pointView(atPort: closedPort())
case "slow":
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
        finish(8)
    }
    let firstPaintElapsed = Date().timeIntervalSince(readyAt)
    guard let firstFrame = stats(of: firstRep) else {
        note("FAIL — could not decode the captured bitmap")
        silent?.stop()
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

default:
    break
}

view.stopAnimation()
silent?.stop()

if failures.isEmpty {
    note("PASS — \(mode)")
    finish(0)
}
for failure in failures { note("FAIL — \(failure)") }
finish(9)
