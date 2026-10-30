Fixed

- the uncached-palace console metrics test waits for the aborted KgWriter task before it reads the disk, which removes a flaky failure (Refs [#10515](https://github.com/bobmatnyc/trusty-tools/issues/10515))
