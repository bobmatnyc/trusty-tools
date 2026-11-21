// Native Cocoa grant-check fixture (JXA): text field + button + label.
// Each press increments a counter, updates the label, and appends "click N"
// to the state file (argv[0]). Used because Tk 8.5 ignores AXPress.
// Run: osascript -l JavaScript fixture.js /tmp/grantcheck_state.txt
ObjC.import('Cocoa');

function run(argv) {
  const statePath = argv[0] || '/tmp/grantcheck_state.txt';
  const app = $.NSApplication.sharedApplication;
  app.setActivationPolicy($.NSApplicationActivationPolicyRegular);
  let n = 0;
  const write = (s) => $(s).writeToFileAtomicallyEncodingError(statePath, true, $.NSUTF8StringEncoding, null);
  let log = 'pid ' + $.NSProcessInfo.processInfo.processIdentifier + '\n';
  write(log);

  const win = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer(
    $.NSMakeRect(80, 400, 420, 220), 1 | 2 | 4, $.NSBackingStoreBuffered, false);
  win.title = 'GrantCheck Fixture';
  win.backgroundColor = $.NSColor.colorWithSRGBRedGreenBlueAlpha(0.16, 0.44, 0.86, 1);

  const field = $.NSTextField.alloc.initWithFrame($.NSMakeRect(110, 160, 200, 24));
  field.stringValue = 'fixture text';
  const label = $.NSTextField.labelWithString('clicks=0');
  label.frame = $.NSMakeRect(160, 110, 200, 30);

  ObjC.registerSubclass({
    name: 'GCTarget',
    methods: {
      'press:': {
        types: ['void', ['id']],
        implementation: function () {
          n += 1;
          label.stringValue = 'clicks=' + n;
          log += 'click ' + n + '\n';
          write(log);
        },
      },
    },
  });
  const target = $.GCTarget.alloc.init;
  const button = $.NSButton.buttonWithTitleTargetAction('Press Me', target, 'press:');
  button.frame = $.NSMakeRect(160, 50, 100, 32);

  [field, label, button].forEach((v) => win.contentView.addSubview(v));
  win.makeKeyAndOrderFront(null);
  app.run;
}
