Changed
- The `default_http_port_does_not_collide_with_known_siblings` guard no longer reserves 7880 for `trusty-mpm`: the daemon serves its Unix socket only and binds no TCP port since #6288 S3a (#6288).
