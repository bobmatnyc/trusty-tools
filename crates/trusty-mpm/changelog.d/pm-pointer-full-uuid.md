Changed

- The PM's cross-session pointer rule now requires every session a pointer addresses or signs to be named by the full UUID `tm session ls` prints, never a short id prefix, because `session_send` rejects anything that is not a UUID.
