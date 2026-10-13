Fixed
- The Python embed sidecar calls `torch.mps.empty_cache()` after every encode on MPS (never on CPU or CUDA), so the MPS allocator no longer keeps the memory of every batch shape it has seen. A missing `torch.mps` is skipped and a failing call is logged once (#9828).
