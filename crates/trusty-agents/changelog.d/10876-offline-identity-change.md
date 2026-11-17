Fixed
- An OKG source read offline in a new process reports a path pending and blocking when its index was re-created or lost content after the path was acknowledged, including after a refused re-push or old-revision remove (#10876). A path re-pushed after the daemon was rolled back to a build without index identities does not read as blocking. Such a read is still never `synced`.
