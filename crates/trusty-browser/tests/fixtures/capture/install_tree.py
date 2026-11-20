"""Print a TSV of a published state tree: rel path, kind, octal mode, sha256 (files) or link target."""
import hashlib
import os
import sys
from pathlib import Path

root = Path(sys.argv[1])
for p in sorted(root.rglob("*")):
    rel = p.relative_to(root)
    st = p.lstat()
    mode = oct(st.st_mode & 0o7777)
    if p.is_symlink():
        print(f"{rel}\tlink\t{mode}\t{os.readlink(p)}")
    elif p.is_dir():
        print(f"{rel}\tdir\t{mode}\t-")
    else:
        print(f"{rel}\tfile\t{mode}\t{hashlib.sha256(p.read_bytes()).hexdigest()}")
