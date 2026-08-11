Changed
- The webhook module's docs no longer describe `trusty-review`'s and `trusty-analyze`'s direct HTTP webhook routes as live. #5181 deleted both, so `POST /api/webhooks/{source}` is now the only HTTP webhook surface in the workspace and the only holder of the shared secret.
