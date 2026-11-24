// Signed native fixture for the bundle-id scoping check (issue #11697).
// Unlike GrantCheck.app (a shell stub that execs osascript), the bundle's own
// main executable owns the window, so LaunchServices and the code signature
// both attest bundle id com.trusty.spike.signedcheck.
// State file (argv[1], or $SIGNEDCHECK_STATE): "pid N", "window W", then one
// "click N" per press of the "Press Me" button; "drag N", "shrunk", "moved" (#11696).
import Cocoa

let args = CommandLine.arguments
let statePath = args.count > 1 && !args[1].hasPrefix("-")
    ? args[1]
    : (ProcessInfo.processInfo.environment["SIGNEDCHECK_STATE"] ?? "/tmp/signedcheck_state.txt")
var log = "pid \(ProcessInfo.processInfo.processIdentifier)\n"
func flush() { try? log.write(toFile: statePath, atomically: true, encoding: .utf8) }
flush()

final class Target: NSObject {
    var n = 0
    let label: NSTextField
    init(label: NSTextField) { self.label = label }
    @objc func press(_ sender: Any?) {
        n += 1
        label.stringValue = "clicks=\(n)"
        log += "click \(n)\n"
        flush()
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
// #11696 Phase B: SIGNEDCHECK_RESIZABLE=1 makes the window resizable (settable AXSize).
var style: NSWindow.StyleMask = [.titled, .closable, .miniaturizable]
if ProcessInfo.processInfo.environment["SIGNEDCHECK_RESIZABLE"] == "1" { style.insert(.resizable) }
let win = NSWindow(contentRect: NSMakeRect(520, 400, 420, 220),
                   styleMask: style,
                   backing: .buffered, defer: false)
#if IMPOSTOR
win.title = "Impostor Fixture"
#else
win.title = "SignedCheck Fixture"
#endif
let label = NSTextField(labelWithString: "clicks=0")
label.frame = NSMakeRect(160, 110, 200, 30)
let target = Target(label: label)
let button = NSButton(title: "Press Me", target: target, action: #selector(Target.press(_:)))
button.frame = NSMakeRect(160, 50, 100, 32)
win.contentView?.addSubview(label)
win.contentView?.addSubview(button)
// #11694: SIGNEDCHECK_SECURE=1 adds a filled password field; a screenshot must be refused.
if ProcessInfo.processInfo.environment["SIGNEDCHECK_SECURE"] == "1" {
    let secret = NSSecureTextField(frame: NSMakeRect(20, 170, 180, 24))
    secret.stringValue = "fixture-secret"
    win.contentView?.addSubview(secret)
}
// #11695: SIGNEDCHECK_MENU=1 adds a safe menu item ("menu N" in the state
// file) beside denied ones (Quit, Close Window) for the live menu check.
if ProcessInfo.processInfo.environment["SIGNEDCHECK_MENU"] == "1" {
    final class MenuTarget: NSObject {
        var n = 0
        @objc func safe(_ sender: Any?) {
            n += 1
            log += "menu \(n)\n"
            flush()
        }
    }
    let menuTarget = MenuTarget()
    objc_setAssociatedObject(app, "menuTarget", menuTarget, .OBJC_ASSOCIATION_RETAIN)
    let bar = NSMenu()
    let appItem = NSMenuItem()
    let appMenu = NSMenu(title: "SignedCheck")
    appMenu.addItem(withTitle: "Quit SignedCheck",
                    action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
    appItem.submenu = appMenu
    bar.addItem(appItem)
    let fixtureItem = NSMenuItem(title: "Fixture", action: nil, keyEquivalent: "")
    let fixtureMenu = NSMenu(title: "Fixture")
    let safe = NSMenuItem(title: "Safe Item", action: #selector(MenuTarget.safe(_:)),
                          keyEquivalent: "")
    safe.target = menuTarget
    fixtureMenu.addItem(safe)
    fixtureMenu.addItem(withTitle: "Close Window",
                        action: #selector(NSWindow.performClose(_:)), keyEquivalent: "")
    fixtureItem.submenu = fixtureMenu
    bar.addItem(fixtureItem)
    app.mainMenu = bar
}
// #11696: SIGNEDCHECK_POINTER=1 adds a drag pad ("drag N" per press-drag-release),
// SIGUSR1 shrinks the window to 200x100 and SIGUSR2 moves it by (60, -40).
if ProcessInfo.processInfo.environment["SIGNEDCHECK_POINTER"] == "1" {
    final class DragPad: NSView {
        var n = 0
        var dragged = false
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
        override func mouseDown(with event: NSEvent) { dragged = false }
        override func mouseDragged(with event: NSEvent) { dragged = true }
        override func mouseUp(with event: NSEvent) {
            if dragged {
                n += 1
                log += "drag \(n)\n"
                flush()
            }
        }
    }
    win.contentView?.addSubview(DragPad(frame: NSMakeRect(10, 10, 140, 100)))
    signal(SIGUSR1, SIG_IGN)
    signal(SIGUSR2, SIG_IGN)
    let shrink = DispatchSource.makeSignalSource(signal: SIGUSR1, queue: .main)
    shrink.setEventHandler {
        var f = win.frame
        f.size = win.frameRect(forContentRect: NSMakeRect(0, 0, 200, 100)).size
        win.setFrame(f, display: true)
        log += "shrunk\n"
        flush()
    }
    shrink.resume()
    let move = DispatchSource.makeSignalSource(signal: SIGUSR2, queue: .main)
    move.setEventHandler {
        win.setFrameOrigin(NSMakePoint(win.frame.origin.x + 60, win.frame.origin.y - 40))
        log += "moved\n"
        flush()
    }
    move.resume()
    objc_setAssociatedObject(app, "pointerSources", [shrink, move], .OBJC_ASSOCIATION_RETAIN)
}
win.makeKeyAndOrderFront(nil)
log += "window \(win.windowNumber)\n"
flush()
app.run()
