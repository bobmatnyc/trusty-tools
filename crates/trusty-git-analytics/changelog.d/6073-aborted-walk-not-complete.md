Fixed

- A history walk whose revwalk stops early — a corrupt or unreadable object — now fails the repository's collect stage instead of returning as a completed walk. The rows it already wrote are kept, but the partial traversal is never recorded as complete, so the next collect re-walks rather than skipping on it. (#6073)
