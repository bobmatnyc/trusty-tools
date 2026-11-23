// Signed native fixture for the bundle-id scoping check (issue #11697).
// Unlike GrantCheck.app (a shell stub that execs osascript), the bundle's own
// main executable owns the window, so LaunchServices and the code signature
// both attest bundle id com.trusty.spike.signedcheck.
// State file (argv[1], or $SIGNEDCHECK_STATE): "pid N", "window W", then one
// "click N" per press of the "Press Me" button.
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
let win = NSWindow(contentRect: NSMakeRect(520, 400, 420, 220),
                   styleMask: [.titled, .closable, .miniaturizable],
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
win.makeKeyAndOrderFront(nil)
log += "window \(win.windowNumber)\n"
flush()
app.run()
