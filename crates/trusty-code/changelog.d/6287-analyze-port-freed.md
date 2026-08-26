Changed

- `tcode serve --http`'s `known_siblings` port-collision guard drops its 7879
  row: trusty-analyze no longer binds a port (#6287, ADR-0032), and a guard
  naming a port nothing binds refuses a value that is free.
