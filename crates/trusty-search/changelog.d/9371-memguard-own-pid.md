Performance
- The memory guard's RSS sample on Linux now refreshes only the sampled pid, and only its memory. It used to load every host process's cmd, environ, exe, cwd and root first, on every memory-pressure tick (every 30 s by default). The reported RSS value is unchanged (#9371).
