Changed

- `SendReading` is no longer `Copy` or `Clone`, and `state` / `accepts_input` borrow it, so one reading allows one send ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
