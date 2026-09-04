// PaintHarness — what the saver actually PAINTS, as an exit code (#6838, #6839).
//
// Why: `LoadHarness.swift` proves the happy path — bundle loads, principal class
//   resolves, the console page finishes. It says nothing about the three states
//   an operator actually complained about: the daemon down, the daemon bound but
//   not yet answering, and the System Settings gallery tile. All three are
//   "whatever the view paints when there is no live page", and all three were
//   reported as a black screen. Nothing measured them, because measuring them
//   means reading pixels, not navigation callbacks.
// What: three modes, each instantiating the bundle's principal class offscreen
//   and reading its rendered bitmap through
//   `bitmapImageRepForCachingDisplay` / `cacheDisplay`:
//     offline — points the view at a closed port; asserts the frame is not black
//               and carries real content within one second of `startAnimation()`.
//     slow    — points the view at a listener that ACCEPTS and never answers (a
//               daemon that has bound its socket mid-restart); asserts the same
//               within one second, then counts connection attempts to prove the
//               load times out and retries instead of hanging.
//     preview — instantiates with `isPreview: true`; asserts the bundled static
//               asset is what draws, and that no web view is built for a tile.
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
/// 2% sits between the two states it has to tell apart. The text wordmark is
/// ~24 monospace glyphs at 3.5% of the view's height: about 0.3-0.5% of the
/// frame. The bundled dashboard render, letterboxed into the view and drawn at
/// 35% while offline, keeps every pixel whose source differs from the
/// background by more than ~69 (the 24 threshold divided back through the 0.35
/// blend) — the clock, the headings, the table text and the graph bars, which
/// is several percent of the frame. Raise it only against a measured frame.
let minInkRatio = 0.02
/// How long after `startAnimation()` the first non-black, non-empty frame is
/// allowed to take. #6838's acceptance says one second.
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

// MARK: - Arguments

let args = CommandLine.arguments
let mode = args.count > 1 ? args[1] : ""
let bundlePath = args.count > 2
    ? args[2]
    : NSHomeDirectory() + "/Library/Screen Savers/TrustyConsole.saver"

func note(_ message: String) {
    FileHandle.standardError.write("PAINT: \(message)\n".data(using: .utf8)!)
}

guard ["offline", "slow", "preview"].contains(mode) else {
    note("usage: paintharness <offline|slow|preview> [bundlePath]")
    exit(64)
}

// MARK: - Defaults override (restored before exit)

let defaultsDomain = "com.trusty.console.saver"
let portKey = "ConsolePort"
let pathKey = "ConsolePath"
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
/// runs `draw(_:)` into the rep synchronously, so this measures the drawing code
/// and nothing about the compositor.
func measure(_ view: NSView) -> PaintStats? {
    guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return nil }
    view.cacheDisplay(in: view.bounds, to: rep)
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

    var nonBlack = 0
    var ink = 0
    var index = 0
    let total = width * height
    while index < total {
        let offset = index * 4
        let r = Int(buffer[offset])
        let g = Int(buffer[offset + 1])
        let b = Int(buffer[offset + 2])
        if max(r, max(g, b)) > 8 { nonBlack += 1 }
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
default:
    break // preview never touches the network
}

// MARK: - Instantiate offscreen

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let frame = NSRect(x: 0, y: 0, width: 1280, height: 800)
let isPreview = mode == "preview"
guard let view = saverClass.init(frame: frame, isPreview: isPreview) else {
    note("init(frame:isPreview:) returned nil")
    finish(5)
}

let window = NSWindow(contentRect: frame, styleMask: [.borderless], backing: .buffered, defer: false)
window.contentView = view
window.orderFrontRegardless()
window.setFrameOrigin(NSPoint(x: -5000, y: -5000)) // offscreen: do not disturb the operator

var failures: [String] = []

let startedAt = Date()
view.startAnimation()
// Half a second of run loop, then measure — comfortably inside the one-second
// budget, and enough for any first-frame invalidation to have been serviced.
RunLoop.current.run(until: startedAt.addingTimeInterval(0.5))

guard let firstFrame = measure(view) else {
    note("FAIL — could not read the view's bitmap")
    view.stopAnimation()
    silent?.stop()
    finish(8)
}
let firstPaintElapsed = Date().timeIntervalSince(startedAt)
note("first frame at \(String(format: "%.2f", firstPaintElapsed))s: \(firstFrame.summary)")

if firstPaintElapsed > firstPaintDeadline {
    failures.append(String(format: "first frame took %.2fs, budget %.2fs", firstPaintElapsed, firstPaintDeadline))
}
if firstFrame.nonBlackRatio < minNonBlackRatio {
    failures.append(String(format: "frame is black: nonBlack=%.4f < %.4f", firstFrame.nonBlackRatio, minNonBlackRatio))
}
if firstFrame.inkRatio < minInkRatio {
    failures.append(String(format: "no static fallback drawn: ink=%.4f < %.4f", firstFrame.inkRatio, minInkRatio))
}

// MARK: - Per-mode assertions

switch mode {
case "preview":
    // A tile must not cost a WebContent XPC child; the asset is the whole point.
    if view.subviews.contains(where: { $0 is WKWebView }) {
        failures.append("preview built a WKWebView")
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
    if let lateFrame = measure(view) {
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
