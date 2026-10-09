Added
- `/health` lists open palaces whose HNSW vector store tripped its op breaker as `hnsw_wedged_palaces` (id plus abandoned-operation count), and reports `status: "wedged"` while any is listed (#9487).
