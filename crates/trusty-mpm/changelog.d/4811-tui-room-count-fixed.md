Fixed

- The TUI health panel's palace detail showed "Wings: N" from a field that has
  always carried a ROOM count (closes
  [#4811](https://github.com/bobmatnyc/trusty-tools/issues/4811), ADR-0027 C3.4).
  It now reads the daemon's truthful `room_count` and renders "Rooms: N",
  falling back to the legacy `wing_count` when talking to a daemon that predates
  the change — where that field held exactly this number.
