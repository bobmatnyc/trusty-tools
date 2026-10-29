Added
- `exec_stub::write_executable(path, contents)` writes a mode-0755 executable through a `/bin/cp` child, so this process never holds a write fd to it and a sibling fork cannot make the later exec fail with ETXTBSY. Every test that writes and then execs a stub uses it (#10248).
