Fixed

- M005 no longer strips the vectors from chunks whose text duplicates another chunk's. Each copy keeps a vector of its own, the orphan sweep no longer deletes a vector it just re-pointed, and the reported `unembedded` count is read from the vector store, so it matches the real gap the embed backfill closes ([#9447](https://github.com/bobmatnyc/trusty-tools/issues/9447))
