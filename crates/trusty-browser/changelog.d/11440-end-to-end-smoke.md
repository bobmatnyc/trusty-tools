Added
- An end-to-end smoke test drives the real `trusty-browser` and `trusty-browser-mcp` binaries in temp directories through serve, native-host pairing, connect, `tools/list`, one page op over a fake worker and a clean shutdown; a `test-seams` build of `serve` reads `TRUSTY_BROWSER_TEST_SEAMS=1` to use an in-memory key store and an open enrollment check, and a release build has no such switch
