Fixed

- `search_uds::routes`' slow-open test builds its oversized request frame from `trusty_common::uds::SOCKET_BUFFER_BYTES` instead of a 512 KiB literal. `uds` now sizes both socket buffers to 1 MiB, so the old figure fit entirely in the kernel, the client's `write_all` returned without parking, and the test stopped proving that the open and the first-frame read share one deadline ([#6896](https://github.com/bobmatnyc/trusty-tools/issues/6896))
