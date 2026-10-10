Fixed
- The search dashboard enables chat under the console exactly when the daemon reports `chat_available: true` in `/health`; it no longer claims no socket method serves `/chat` (Refs #9030).
