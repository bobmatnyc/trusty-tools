Fixed

- give each `migrations::file_stamp` test its own `tempfile::TempDir`. The old scratch-directory name was `trusty-common-stamp-test-{pid}-{nanos}`, and every test in the module runs in one process, so two tests that started inside the same clock tick shared a directory and overwrote each other's stamp file. Test-only; no production behaviour changed
