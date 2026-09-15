Security
- The Gmail wake no longer carries a sender's `From:` display name into a `UserIdentity` verbatim. Control characters are dropped, the value is trimmed and capped at 128 characters, so a hostile sender cannot forge a log record or run an ANSI sequence through any consumer that writes the identity out.
