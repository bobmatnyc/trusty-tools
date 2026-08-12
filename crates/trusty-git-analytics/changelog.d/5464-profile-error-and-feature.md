Changed

- `profile::ProfileError` gains an `Inference` variant wrapping `trusty_common::inference::InferenceError`, raised when `PeriodReviewer::from_slug` finds no credential or no registered factory for a slug's provider family. The enum is `#[non_exhaustive]`, so this is additive.
- tga enables trusty-common's `inference-client` feature explicitly. `config-cli` already implied it; naming it makes the profile transport's dependency deliberate rather than a side effect of the `config` subcommand staying mounted.
