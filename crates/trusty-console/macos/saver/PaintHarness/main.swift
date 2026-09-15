// PaintHarness — what the saver actually PAINTS, as an exit code (#6838, #6839).
//
// Why: `LoadHarness.swift` proves the happy path — bundle loads, principal class
//   resolves, the console page finishes. It says nothing about the three states
//   an operator actually complained about: the daemon down, the daemon bound but
//   not yet answering, and the System Settings gallery tile. All three are
//   "whatever the view paints when there is no live page", and all three were
//   reported as a black screen. Nothing measured them, because measuring them
//   means reading pixels, not navigation callbacks.
// What: twelve modes, each instantiating the bundle's principal class offscreen
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
//     occluded  #7846: the never-answering listener with the view's visibility
//               verdict FORCED to occluded. Asserts a load that outlives the
//               6 s deadline behind an occluded window neither fails nor
//               retries, and then that flipping the verdict to visible produces
//               the failure and the retry after all.
//     occluded- #7846: a listener that hangs up instead, so every attempt fails
//     failing   for real while occluded. Asserts the retry backoff still runs
//               and the WKWebView instance is never replaced — a rebuild into
//               the same occluded window is the loop the issue reports.
//     visibility #7846: the never-answering listener with the verdict forced to
//     -unknown  UNKNOWN. Asserts the view waits — no rebuild where the unfixed
//               one rebuilds — but only for a bounded number of re-asks, after
//               which it judges loads again and does reach the rebuild.
//   Every mode takes the frame it runs at — `--frame WxH`, default 1280x800 —
//   so the ultrawide geometry #6871 was reported on is reachable.
// Test: it IS the test. README.md, "Paint harness", has the invocations;
//   `scripts/build-console-saver.sh` builds the bundle it consumes.
//
// Like `LoadHarness.swift` it runs UNSANDBOXED, so it proves the drawing code,
// not the sandboxed `legacyScreenSaver.appex` host. The in-host run stays manual.
//
// Layout (#7856): this file is the entry point and the per-mode dispatch.
// `Thresholds.swift` holds the numbers, `Endpoints.swift` the test listeners,
// `PixelMeasurement.swift` the bitmap reads, and `LoadModes.swift` and
// `RecoveryModes.swift` the per-mode assertions. swiftc runs top-level code
// only from a file named `main.swift` once a build has more than one file.

import AppKit
import Foundation
import Network
import ScreenSaver
import WebKit

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
       "recreate", "one-failure", "occluded", "occluded-failing",
       "visibility-unknown"].contains(mode) else {
    note("usage: paintharness <offline|slow|preview|resize|stop|suspend|suspend-cold"
        + "|recreate|one-failure|occluded|occluded-failing|visibility-unknown>"
        + " [bundlePath] [--frame WxH] [--start WxH]")
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
case "occluded-failing":
    // #7846: every attempt has to fail for a REAL reason while the window is
    // occluded — the rebuild this mode must not reach is counted off hard
    // failures, not off the deadline. A listener that accepts and then hangs up
    // produces one per attempt AND counts them, which a closed port cannot.
    guard let listener = SilentListener(hangsUp: true) else {
        note("could not start the hang-up listener")
        finish(6)
    }
    silent = listener
    pointView(atPort: listener.port)
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
case "slow", "stop", "recreate", "one-failure", "occluded", "visibility-unknown":
    // #6900 wants the same endpoint `slow` uses: a load that is in flight and
    // stays there is the state the stop has to interrupt, and every connection
    // the view opens is counted. #7606's two modes want it for a third reason —
    // it is the only endpoint that makes every load fail the same way, so the
    // failure COUNT is the variable under test. #7846's two want it for a
    // fourth: a load that is still in flight when the deadline elapses is the
    // exact shape an occluded window produces, and the connection count is how
    // the harness sees the view either give up on it or wait.
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

// MARK: - Window-visibility verdict (#7846)

/// Force the view's window-visibility verdict, or report that the bundle has no
/// seam to force it through.
///
/// AppKit reports no `.visible` for the window above — it is parked off every
/// display — so a view left to ask AppKit runs every mode as occluded. Before
/// #7846 that made no difference; now it decides whether a load deadline counts,
/// so every mode that asserts the deadline path has to say which verdict it
/// means. A bundle built before #7846 answers `false` here rather than raising
/// an ObjC exception on an unknown key.
///
/// #7856: `view` is a parameter because `RecoveryModes.swift` calls this too, and
/// a function another file calls may not capture a top-level `guard let` binding.
func forceVisibility(_ verdict: Int, of view: ScreenSaverView) -> Bool {
    guard view.responds(to: NSSelectorFromString(visibilitySetterSelector)) else { return false }
    view.setValue(verdict, forKey: "windowVisibilityOverride")
    return (view.value(forKey: "windowVisibilityOverride") as? Int) == verdict
}

/// What each mode needs the view to believe. The modes that predate #7846 take
/// `visible`, which is the verdict their assertions were written against; the
/// three #7846 modes take the state under test. `suspend` and `suspend-cold`
/// are deliberately absent: their #7112 assertions turn on the view's own
/// occluded-window reasoning, so forcing a verdict there would change what they
/// measure.
let forcedVisibility: Int? = {
    switch mode {
    case "slow", "stop", "recreate", "one-failure": return visibilityVisible
    case "occluded", "occluded-failing": return visibilityHidden
    case "visibility-unknown": return visibilityUnknown
    default: return nil
    }
}()
/// Whether the seam took. Only the #7846 modes fail on its absence — for the
/// older modes the forced verdict merely restores what they measured before it
/// existed, and an old bundle has the old behaviour to measure.
var visibilitySeamPresent = true
if let forcedVisibility {
    visibilitySeamPresent = forceVisibility(forcedVisibility, of: view)
    note("window-visibility verdict forced to \(forcedVisibility):"
        + " \(visibilitySeamPresent ? "accepted" : "NO SEAM in this bundle (pre-#7846)")")
}

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
    assertResizeMode(view)

case "slow":
    assertSlowMode(view)

case "recreate", "one-failure":
    assertRebuildModes(view)

case "stop":
    assertStopMode(view)

case "suspend", "suspend-cold":
    assertSuspendModes(view)

case "occluded", "occluded-failing", "visibility-unknown":
    assertOcclusionModes(view)

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
