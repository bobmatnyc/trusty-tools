Fixed

- Enter pressed while a turn is in flight now records the newline it stands for instead of being dropped, so type-ahead queued across several lines submits byte-identical to what was typed rather than welded together (`echo twoecho three`); the composer shows each queued break as `⏎` (refs [#8240](https://github.com/bobmatnyc/trusty-tools/issues/8240))
- the event loop applies every event already queued behind the one it received before redrawing, instead of one full frame per event — a streaming turn no longer outruns the render path and leaves the pane minutes behind the daemon (refs [#8240](https://github.com/bobmatnyc/trusty-tools/issues/8240))
