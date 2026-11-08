Fixed
- A pinned session's gh shim now passes a TERM, INT or HUP it receives on to the buffered gh call at once, waits for gh to exit, removes its temp buffer and exits 143, 130 or 129. Before, the shim acted on the signal only after gh finished, so a supervisor that sent TERM and then KILL left gh running and the buffer on disk (#10550).
