Changed

- The `[report].findings` writer the CVE (#6075) and license (#6076) collectors
  each carried privately is now one shared `grounding::findings::append`, taking
  the row-identity key set as a parameter. Behaviour is unchanged; the third
  copy #6077 would have added does not exist.
