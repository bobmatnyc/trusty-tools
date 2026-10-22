Breaking
- A `message` event says which end recorded it: `MessageEvent` has a new `direction` field (`sent` or `received`, the new `MessageDirection`). A frame written without one reads as `sent`. Code that builds a `MessageEvent` with a struct literal must set the field (#9167).
