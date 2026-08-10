Fixed

- A cold index whose corpus open lost a race (`DatabaseAlreadyOpen`) or ran past
  its deadline is no longer deregistered for the daemon's lifetime. The restore
  ran to completion without registering a handle, so the loader consumed the
  cold entry anyway and the index became a 404 with no way back — one client
  retry against a slow ~20s cold-start open was enough. Registration is now the
  only evidence a restore succeeded; a transient failure keeps the entry and
  returns a retryable 503 so the next query recovers it.
