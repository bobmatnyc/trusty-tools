Fixed
- The `app_quit` tests write their fake `osascript` through `trusty_common::exec_stub::write_executable`, so a sibling test's fork can no longer make its exec fail with `ETXTBSY`; a non-timeout error in `system_quitter_timeout_before_check_is_an_error` is now printed in the failure message (#11990).
