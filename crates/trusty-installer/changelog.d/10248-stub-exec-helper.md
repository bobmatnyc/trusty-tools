Changed
- Test stubs that are written and then exec'd now go through `trusty_common::exec_stub::write_executable`, which removes the ETXTBSY race on Linux CI (#10248). Test-only; no runtime change.
