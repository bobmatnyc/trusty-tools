Fixed
- Opening one of the dashboard's Server-Sent Event streams is now bounded at 60
  seconds total. A trusty-search daemon that accepts the connection and then
  answers nothing — a full listener backlog — used to leave the browser waiting
  out the 24-hour per-frame budget for a response head; it now answers `502`
  with the reason. The dial, the request write and the first frame read share
  one deadline, so a slow-but-successful open leaves the first read what is left
  of the 60 seconds rather than a fresh 60 of its own. An established stream
  keeps the long per-frame budget, so a reindex that emits nothing for minutes
  is still not cut off. The bridge also stops reading from the socket the moment
  the browser disconnects, rather than at the next frame, which releases the
  daemon's producer on a stream that is quiet (#6285).
