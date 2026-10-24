Breaking
- `daemon::api::HealthResponse` and `client::http_client::HealthSnapshot` gain `unbound_delegations` and `unbound_delegations_error` (#9007). Code that builds either with a struct literal must set both (`None` for a count the caller did not take).
