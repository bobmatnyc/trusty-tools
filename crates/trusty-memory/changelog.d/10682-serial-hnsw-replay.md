Fixed
- A palace open can no longer hang: the HNSW graph replay inserts one vector at a time, because a parallel `hnsw_rs` insert can deadlock on two points that are each other's neighbours (#10682). A large palace now takes longer to open.
