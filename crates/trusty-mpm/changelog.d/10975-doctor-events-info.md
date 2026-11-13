Changed
- `tm doctor`'s `events` row on a host without the trusty-events bus is now an INFO line, `info: trusty-events: not installed (internal, optional)`, with no fix command. It no longer counts as a warning (#10975).
- The `events` row's version-skew remedy no longer names `tctl upgrade trusty-events`, which skips the unpublished bus. It now says to rebuild the bus from a trusty-tools checkout at the tm revision, then run `tctl restart trusty-events` (#10975).
