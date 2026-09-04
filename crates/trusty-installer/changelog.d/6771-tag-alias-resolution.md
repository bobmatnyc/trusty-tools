Fixed

- Release-tag resolution accepts either tag spelling for a crate whose tag prefix differs from its package name, so a `tga` pin resolves the `trusty-git-analytics-v<version>` tag the publish gate pushes instead of reporting the release as unpublished (#6771).
- When one version exists under both spellings and they name different commits or asset digests, resolution fails with a TAG-SPLIT error naming both tags rather than picking one.
- The "not a published stable release" message lists versions found under either spelling.
