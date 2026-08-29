Fixed
- A console request to a trusty-search endpoint with no socket method answers
  `501` naming the endpoint, and a daemon that is not listening answers `502`
  with the reason. Neither reaches the dashboard as an empty success, so "the
  daemon is down" stays distinguishable from "the daemon has nothing to show".
  `POST /chat` and `POST /admin/stop` are the two endpoints the search dashboard
  calls that have no socket method yet (#6285).
