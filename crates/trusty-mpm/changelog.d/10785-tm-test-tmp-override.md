Added
- Test fixtures honor a `TM_TEST_TMP` environment variable that moves the `tm-test-*` temp root off `/tmp`; `TMPDIR` stays ignored and a root inside `$HOME` still panics (#10785).
