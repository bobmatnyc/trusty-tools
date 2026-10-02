Added

- `trusty-memory doctor` warns when the daemon reports a palace lock held past a stall threshold, naming the lock, the palace and how long it has been held. The threshold defaults to the write-lock timeout (60 s) and is set with `TRUSTY_DOCTOR_LOCK_STALL_SECS`; below it the "N in flight, oldest Ns" line is unchanged, and a held-lock report doctor cannot read is "undetermined", not a pass ([#8751](https://github.com/bobmatnyc/trusty-tools/issues/8751))
