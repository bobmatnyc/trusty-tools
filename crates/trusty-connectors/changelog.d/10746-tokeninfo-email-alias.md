Fixed
- The live lane's scope pre-check accepts tokeninfo's bare `email`, which Google reports for the read-only preset's `userinfo.email`; before, it refused every correctly minted read-only token. Bare `profile` still refuses, because the preset does not request `userinfo.profile` (#10746).
