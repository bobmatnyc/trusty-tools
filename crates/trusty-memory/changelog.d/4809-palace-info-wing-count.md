Fixed

- `PalaceInfo.wing_count` reports the real number of wings (ADR-0027 T9, [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - #4811 replaced the old room-count-under-a-wing-label with a hardcoded `1`,
    correct while `DEFAULT_WING_ID` was the only wing the model could hold. T9's
    `wing_create` made more than one possible, so the constant became the same
    category of lie the field carried before
  - it is now read from the `WINGS` registry — the same source `wing_list` uses —
    so the number and the tool surface cannot disagree
  - `0` still means unknown for an uncached palace (#4637); an open palace whose
    registry cannot be read degrades to `1`, never to `0`
  - `palace_info` gains `wing_count` alongside the `room_count` #4807 added
