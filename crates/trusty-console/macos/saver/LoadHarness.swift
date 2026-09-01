// LoadHarness — bundle-load smoke test for TrustyConsole.saver (#6520).
//
// Why: the two ways a `.saver` bundle silently fails are (a) `NSPrincipalClass`
//   not resolving to the Swift class, and (b) the WKWebView never reaching the
//   console. Both are invisible in a normal screen-saver run — the screen just
//   stays dark. This harness makes each one an exit code.
// What: loads the bundle at `argv[1]`, resolves its principal class, instantiates
//   it offscreen, calls `startAnimation()`, and waits for a `didFinish` os_log
//   entry by polling the view's live/offline state through its web view.
// Test: it IS the test. `scripts/build-console-saver.sh` builds the bundle it
//   consumes; README.md, "Smoke test", has the invocation.
//
// It runs UNSANDBOXED, so a pass here proves the bundle and the URL are sound but
// says nothing about the sandboxed `legacyScreenSaver.appex` host. The in-host run
// stays manual.

import AppKit
import ScreenSaver
import WebKit

let args = CommandLine.arguments
let bundlePath = args.count > 1
    ? args[1]
    : NSHomeDirectory() + "/Library/Screen Savers/TrustyConsole.saver"
// Optional second argument: the console URL to point the view at, e.g.
// http://127.0.0.1:7788/ui while #6519's /ui/screensaver route is unmerged.
let overrideURL: URL? = args.count > 2 ? URL(string: args[2]) : nil

func note(_ message: String) {
    FileHandle.standardError.write("HARNESS: \(message)\n".data(using: .utf8)!)
}

// MARK: - Optional defaults override (restored before exit)

let defaultsDomain = "com.trusty.console.saver"
let portKey = "ConsolePort"
let pathKey = "ConsolePath"
let defaults = ScreenSaverDefaults(forModuleWithName: defaultsDomain)
let priorPort = defaults?.object(forKey: portKey)
let priorPath = defaults?.object(forKey: pathKey)

func restoreDefaults() {
    guard let defaults else { return }
    if let priorPort { defaults.set(priorPort, forKey: portKey) } else { defaults.removeObject(forKey: portKey) }
    if let priorPath { defaults.set(priorPath, forKey: pathKey) } else { defaults.removeObject(forKey: pathKey) }
    defaults.synchronize()
}

func finish(_ code: Int32) -> Never {
    restoreDefaults()
    exit(code)
}

if let overrideURL {
    guard let defaults, let port = overrideURL.port else {
        note("override URL needs an explicit port: \(overrideURL.absoluteString)")
        finish(6)
    }
    defaults.set(port, forKey: portKey)
    defaults.set(overrideURL.path.isEmpty ? "/" : overrideURL.path, forKey: pathKey)
    defaults.synchronize()
    note("override applied: \(portKey)=\(port) \(pathKey)=\(overrideURL.path)")
}

// MARK: - Bundle load

guard let bundle = Bundle(path: bundlePath) else {
    note("Bundle(path:) returned nil for \(bundlePath)")
    finish(2)
}
note("bundle=\(bundlePath) loaded=\(bundle.load())")

guard let principal = bundle.principalClass else {
    note("principalClass is nil — NSPrincipalClass did not resolve")
    finish(3)
}
note("principalClass=\(principal)")

guard let saverClass = principal as? ScreenSaverView.Type else {
    note("principalClass is not a ScreenSaverView subclass")
    finish(4)
}

// MARK: - Instantiate and animate offscreen

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let frame = NSRect(x: 0, y: 0, width: 1280, height: 800)
guard let view = saverClass.init(frame: frame, isPreview: false) else {
    note("init(frame:isPreview:) returned nil")
    finish(5)
}

let window = NSWindow(contentRect: frame, styleMask: [.borderless], backing: .buffered, defer: false)
window.contentView = view
window.orderFrontRegardless()
window.setFrameOrigin(NSPoint(x: -5000, y: -5000)) // offscreen: do not disturb the operator
view.startAnimation()
note("startAnimation called; waiting up to 20s for the web view to finish loading")

// The saver hides its web view until `didFinish` fires, so `isHidden == false` is
// the observable form of "the console loaded".
let deadline = Date().addingTimeInterval(20)
var loaded = false
while Date() < deadline && !loaded {
    RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    if let web = view.subviews.compactMap({ $0 as? WKWebView }).first {
        loaded = !web.isHidden
    }
}

if let web = view.subviews.compactMap({ $0 as? WKWebView }).first {
    note("web view url=\(web.url?.absoluteString ?? "<nil>") title=\(web.title ?? "<nil>")")
}
view.stopAnimation()

if loaded {
    note("PASS — web view visible, console load finished")
    finish(0)
}
note("FAIL — web view never finished loading (console down, or the route 404s)")
finish(7)
