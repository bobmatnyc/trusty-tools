Fixed
- Every console call to trusty-memory (the dashboard bridge, its activity stream, and the palace delete and compact actions) now checks the daemon's `memory.protocol` version first and refuses an unsupported or unreadable one with a named 502 or 409 instead of sending the frame (#9538).
