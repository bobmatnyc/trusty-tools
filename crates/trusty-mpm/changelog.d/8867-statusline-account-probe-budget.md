Fixed

- The statusline account-segment test no longer fails under a loaded test run.
  The account probe now takes its time budget and reader as parameters, so the
  test waits for the config read instead of racing the live 100 ms budget. The
  timeout fallback has its own deterministic test. The live statusline still
  gives up on the account read after 100 ms.
