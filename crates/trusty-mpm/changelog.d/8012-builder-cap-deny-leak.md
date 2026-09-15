Fixed

- A builder-cap deny whose delegation record had not landed yet no longer leaks a live `Running` record for a dispatch that never ran: the builder-slot route's refusal now releases through the delegation tracker, which leaves the `Cancelled` tombstone a late writer finds, instead of writing nothing when the record is absent. Such a record held one of the machine's builder slots for the 45 minutes of the builder lease TTL, and an unisolated one occupied the checkout for six hours.
