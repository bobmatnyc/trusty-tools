Changed
- `tm doctor`'s `events` row on a host without the trusty-events bus is now an INFO line, `info: trusty-events: not installed (internal, optional)`, with no fix command. It no longer counts as a warning. It appears only when there is no socket, no LaunchAgent plist, no `trusty-events` binary on `PATH` or in `~/.cargo/bin`, and no loaded launchd label (#10975).
- With no socket and no plist, but the binary present or the label loaded, the `events` row warns that the bus is not running instead of reading as not installed (#10975).
- The `events` row's version-skew warning names the bus as a build older than #10071 and this tm's version. It no longer names `tctl upgrade trusty-events`, which skips the unpublished bus, or any install command (#10975).
