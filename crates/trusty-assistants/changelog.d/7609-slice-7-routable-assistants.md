Added
- `GET /api/channels` answers with `routable_assistants`, the assistant names a global channel's `route_to` may name. It is served from the same host enumeration the inbound dispatcher measures `route_to` against, so a client no longer keeps a roster of its own that can drift into unexplainable 400s.
