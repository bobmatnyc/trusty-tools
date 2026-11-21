Fixed

- `tm fleet init` reads a `local/state.md` line as a hold only when it starts with a hold format (`HOLD: …`, `usage state: HOLD …`, `On hold …`). A note that only uses the word "hold" or "merge" no longer stops init or becomes a merge hold. The state.md seed and the Architect instructions show the format.
