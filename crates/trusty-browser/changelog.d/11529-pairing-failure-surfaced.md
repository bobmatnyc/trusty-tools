Fixed

- A stale extension worker recovers when its pairing handshake fails. The broker's refusal frame on `service.sock` now carries the published asset stamp, so the worker reloads on a mismatch. A handshake timeout, or a disconnect before the acknowledgement, shows "Pairing timed out" or "Pairing failed" in the popup status instead of failing silently.
