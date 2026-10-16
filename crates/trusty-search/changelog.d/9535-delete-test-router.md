Removed
- The test-only axum router (`service::server::build_router`, `build_router_with_self_origins`, `build_router_on`) and the handlers only it called: `upgrade_handler`, the two SSE handlers (`status_stream_handler`, `reindex_stream_handler` and its heartbeat constants), `get_config_handler`, `patch_config_handler`, `search_similar_handler`, `graph_stats_handler`, `warm_start_handler`, `warm_status_handler` and `add_index_roots_handler`. The socket serves each `*_report` core they wrapped (#9535).
- The unused `tower-http` dependency (#9535).
