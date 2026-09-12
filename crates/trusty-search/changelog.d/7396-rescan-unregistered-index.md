Fixed

- A dropped-event rescan whose index has no registry handle now re-arms instead of being discarded (#7396). The watch loop answered an absent handle with a bare `continue` — no log line, no failure count, no retry — while the paths the OS dropped were already unrecoverable, so a handle missing for a moment cost the index an unknown set of file changes permanently. The registry lookup moved inside the pass: an absent handle is now `RescanError::UnregisteredIndex`, which flows through the same failure counting and backoff retry every other incomplete pass uses.
