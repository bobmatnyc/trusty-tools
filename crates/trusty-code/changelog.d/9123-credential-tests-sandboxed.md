Fixed

- The daemon-credential tests and the missing-OpenRouter-key test run
  `#[serial]` inside the shared credential sandbox, so the token-file fallback
  reads an empty temp data dir rather than the real daemon's token, and no
  assertion prints a resolved credential (#9123).
