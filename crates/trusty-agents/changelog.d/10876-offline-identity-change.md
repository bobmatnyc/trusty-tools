Fixed
- An OKG source read offline in a new process reports a path pending and blocking when its index was re-created or lost content after the path was acknowledged, including after a refused re-push or old-revision remove (#10876). Such a read is still never `synced`.
