Fixed
- The `CLAUDE.md` seed guard and the automatic `git init` no longer refuse a project directory that has more than 256 subdirectories and no nested repository. Both scans now check up to 16,384 directories, the same budget as `tm fleet init`; a tree too large to check still refuses (#10461).
