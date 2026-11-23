Fixed
- A daemon state with a pinned test home seeds its project registry from that home's `config.yaml`, so isolated test state no longer reads the real `~/.trusty-tools` project list (#11170).
