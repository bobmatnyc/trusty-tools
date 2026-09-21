Fixed

- The two `resume_managed` tests that drive the real launch path now plant a stub `claude` on `PATH`, so they exercise the pane-typing and post-send arms on a machine with no Claude Code install instead of stopping at the adapter's binary lookup and failing on every CI runner (#7862).
