Added

- `session_naming::RESERVED_TEST_PREFIX` (`tm-xtest-`) and
  `is_reserved_test_session_name`, the namespace trusty-mpm's adoption sweep
  refuses (#6116). It sits inside the managed `tm-` prefix, so a session in it
  is still recognised by `is_managed_session_name` and still reapable by the
  orphan-GC; what changes is that no automatic path adopts it into the session
  store. The constant lives here because the daemon that refuses such a name and
  the test fixture that mints one both read it, and a second copy would drift.
