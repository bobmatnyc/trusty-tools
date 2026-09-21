Documentation

- BASE-AGENT states that reusing an already-established credential (for
  example a gcloud application-default credential via
  `CLOUDSDK_AUTH_ACCESS_TOKEN`) for a read-only call is not credential
  switching; a brief forbidding "any login" forbids only initiating a new
  interactive login (#8133)
