Fixed

- `POST /indexes` now refuses a root that sits inside, or encloses, an existing index's root with a `409` naming the conflicting index id and root path (#4289). Containment is decided over `(dev, ino)` per path segment, so a symlink alias is caught and a sibling sharing a name prefix is not. A candidate that cannot be canonicalized is a `500`, never an implicit "no overlap".
