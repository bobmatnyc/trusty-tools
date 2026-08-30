Fixed

- `SearchClient::fetch_all` (the monitor dashboard's trusty-search poller) no
  longer GETs `/indexes/:id/communities` — trusty-search has never served that
  route (the Louvain community-detection pipeline was retired server-side in
  v0.10.0, issue #152), so every call 404'd. `index_communities` and its
  `CommunitiesWire` wire struct are removed; `IndexRow.community_count` /
  `.modularity` stay on the struct (existing TUI tests exercise them directly)
  but nothing populates them from the daemon. (#6382)
