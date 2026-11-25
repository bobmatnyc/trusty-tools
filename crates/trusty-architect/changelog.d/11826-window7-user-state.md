Fixed

- The Rust poll cycle now raises `user_state`: one alert per change of the owner's presence state in the dashboard's `meta/settings`, fingerprinted by `user_state_at`, as `fleet-poll.py`'s `user_state_alerts` does. An unreadable settings document raises one `user_state_error` alert and keeps the `user_state` dedup keys, instead of saying nothing (Refs [#11826](https://github.com/bobmatnyc/trusty-tools/issues/11826))
