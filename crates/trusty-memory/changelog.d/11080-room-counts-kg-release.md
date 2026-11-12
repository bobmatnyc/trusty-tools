Fixed
- the room-counts console metrics test waits for the evicted palace's KgWriter task before it reads the disk, which removes a flaky failure (Refs [#11080](https://github.com/bobmatnyc/trusty-tools/issues/11080))
