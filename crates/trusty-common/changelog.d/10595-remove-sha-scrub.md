Removed
- The public `catchup::sha_scrub` module (`scrub_sha_like`, `is_sha_like`, `SHA_PLACEHOLDER`) is removed; use `catchup::secret_scrub` instead. This is a semver break for any caller of the old module (#10595).
