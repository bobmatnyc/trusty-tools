Fixed
- Tests that write a stub executable and then run it write it through `trusty_common::exec_stub::write_executable`, so a sibling test's fork can no longer fail the run with "Text file busy" (ETXTBSY) (#10248).
