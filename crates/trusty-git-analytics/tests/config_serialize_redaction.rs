//! Every config credential is masked on every serialization path (#5775).
//!
//! Why: `LinearConfig`, `GithubConfig`, `JiraConfig`, `BitbucketConfig`,
//! `AzureDevOpsConfig` and `ClassificationConfig` derived `Serialize` over six
//! plaintext credential fields, so `serde_json::to_string` or
//! `serde_yaml::to_string` of a config section — or of the whole `Config`, which
//! embeds them — wrote live tokens to a file, a manifest, or a response body.
//! #5770 fixed the `Debug` half of the same leak; this is the other half.
//!
//! What: each test loads a config section from YAML exactly as the pipeline
//! does, serializes it back through both JSON and YAML, and asserts three
//! things — the whole credential is absent, its first four characters are
//! absent, and the `<redacted>` marker is present. A non-secret probe field is
//! asserted to survive, so a redactor that blanks everything cannot pass.
//!
//! Two deliberate choices about where and how these tests are written:
//!
//! - They live OUTSIDE the crate and use nothing but the public API, so they
//!   compile unchanged against the pre-fix source. Running them against
//!   `origin/main` is therefore a real fail-open proof rather than a
//!   restatement of the fix.
//! - Every fixture is deserialized from YAML rather than built with a struct
//!   literal. `GithubConfig` and `JiraConfig` are `#[non_exhaustive]`, so an
//!   external crate cannot write a literal for them at all — and loading from
//!   YAML is what a real config does anyway, which makes the assertion
//!   "a credential that came in through the front door does not go back out".
//!
//! Test: itself.

use tga::classify::classifier::ClassificationEngineConfig;
use tga::core::config::{
    AzureDevOpsConfig, BitbucketConfig, ClassificationConfig, Config, GithubConfig, JiraConfig,
    LinearConfig,
};

/// What a masked credential must render as. Duplicated from
/// `core::config::credential_debug::REDACTED` rather than imported: the constant
/// is crate-private, and pinning the marker from outside is what makes it a
/// wire-format contract instead of an implementation detail.
const REDACTED: &str = "<redacted>";

/// A non-secret field value asserted to survive redaction, distinct from every
/// entry in [`SECRET_SHAPES`] so a "the secret leaked" assertion can never be
/// satisfied by this instead.
const PROBE: &str = "probe-value-kept-in-the-clear";

/// Credential shapes worth covering, with why each is here. Mirrors the table
/// `core::config::credential_debug`'s tests use, for the same reason: none of
/// these fields is format-validated, so a mask that echoed a fixed-length head
/// would look safe against a `ghp_`-prefixed token and disclose real entropy
/// against everything else.
///
/// No empty case: an empty credential has nothing to disclose, so asserting on
/// it measures the mask rather than the leak.
const SECRET_SHAPES: &[(&str, &str)] = &[
    (
        "9f3Kq7Zt2Wm4Bx8Lv6Nc1Rd5Ph0Sj",
        "no prefix: entropy up front",
    ),
    (
        "ghp_averyrealisticlookingtoken0123456789",
        "a provider-prefixed token",
    ),
    ("ab7Q", "exactly a four-character head"),
    ("x9", "shorter than a head"),
];

/// Deserialize a config section from YAML, panicking with the YAML on failure.
fn load<T: serde::de::DeserializeOwned>(yaml: &str) -> T {
    serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("fixture did not load: {e}\n{yaml}"))
}

/// Serialize `value` through both formats a config could plausibly be written
/// in, concatenated so one assertion covers both.
///
/// Why both: `serde_json` and `serde_yaml` drive different parts of a
/// `Serialize` impl, and a config is far likelier to be written back as YAML
/// than as JSON.
fn render<T: serde::Serialize>(value: &T) -> String {
    let json = serde_json::to_string(value).expect("json");
    let yaml = serde_yaml::to_string(value).expect("yaml");
    format!("{json}\n{yaml}")
}

/// Assert `rendered` masked `secret` and kept [`PROBE`].
///
/// The leading-fragment check is the point: asserting only that the whole
/// secret is absent passes for a head-echoing redactor, which is the exact
/// disclosure #5733 ruled out for the `Debug` half of this fix.
fn assert_masked(rendered: &str, secret: &str, why: &str) {
    assert!(
        !rendered.contains(secret),
        "{why}: the whole credential reached serialized output: {rendered}"
    );
    let head: String = secret.chars().take(4).collect();
    assert!(
        !rendered.contains(&head),
        "{why}: a leading fragment of the credential survived: {rendered}"
    );
    assert!(
        rendered.contains(REDACTED),
        "{why}: the credential field was not masked: {rendered}"
    );
    assert!(
        rendered.contains(PROBE),
        "{why}: redaction must not cost the non-secret fields: {rendered}"
    );
}

/// Load `T` from `yaml_for(secret)` and assert its rendering, once per shape in
/// [`SECRET_SHAPES`].
fn for_each_shape<T>(yaml_for: impl Fn(&str) -> String)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    for (secret, why) in SECRET_SHAPES {
        let cfg: T = load(&yaml_for(secret));
        assert_masked(&render(&cfg), secret, why);
    }
}

#[test]
fn serialize_never_renders_the_linear_api_key() {
    for_each_shape::<LinearConfig>(|secret| {
        format!("api_key: \"{secret}\"\nticket_regex: \"{PROBE}\"\n")
    });

    let unset: LinearConfig = load(&format!("ticket_regex: \"{PROBE}\"\n"));
    let rendered = serde_json::to_string(&unset).expect("json");
    assert!(
        rendered.contains("\"api_key\":null"),
        "an unset key must stay visibly unset — masking must not invent one: {rendered}"
    );
}

#[test]
fn serialize_never_renders_the_github_token() {
    for_each_shape::<GithubConfig>(|secret| format!("token: \"{secret}\"\norg: \"{PROBE}\"\n"));

    let unset: GithubConfig = load(&format!("org: \"{PROBE}\"\n"));
    let rendered = serde_json::to_string(&unset).expect("json");
    assert!(
        rendered.contains("\"token\":null"),
        "an unset token must stay visibly unset: {rendered}"
    );
}

#[test]
fn serialize_never_renders_the_jira_token() {
    for_each_shape::<JiraConfig>(|secret| format!("token: \"{secret}\"\nurl: \"{PROBE}\"\n"));

    let unset: JiraConfig = load(&format!("url: \"{PROBE}\"\n"));
    let rendered = serde_json::to_string(&unset).expect("json");
    assert!(
        rendered.contains("\"token\":null"),
        "an unset token must stay visibly unset: {rendered}"
    );

    let named: JiraConfig = load(&format!(
        "token: \"ghp_averyrealisticlookingtoken0123456789\"\n\
         username: \"ops@example.com\"\n\
         url: \"{PROBE}\"\n"
    ));
    let rendered = serde_json::to_string(&named).expect("json");
    assert!(
        rendered.contains("ops@example.com"),
        "the JIRA account name is what identifies the run and must survive: {rendered}"
    );
}

/// Bitbucket carries two independent secrets — Basic-auth `app_password` and
/// Bearer `token` — and a migrating config populates both. Each is also checked
/// alone, because a mask that reads the wrong field passes the both-set case by
/// masking whichever it does read.
#[test]
fn serialize_never_renders_either_bitbucket_secret() {
    for_each_shape::<BitbucketConfig>(|secret| {
        format!("app_password: \"{secret}\"\ntoken: \"{secret}\"\nworkspace: \"{PROBE}\"\n")
    });

    for (secret, why) in SECRET_SHAPES {
        let password_only: BitbucketConfig = load(&format!(
            "app_password: \"{secret}\"\nworkspace: \"{PROBE}\"\n"
        ));
        assert_masked(&render(&password_only), secret, why);

        let token_only: BitbucketConfig =
            load(&format!("token: \"{secret}\"\nworkspace: \"{PROBE}\"\n"));
        assert_masked(&render(&token_only), secret, why);
    }
}

/// `pat` is a bare `String`, so there is no unset state to keep visible.
#[test]
fn serialize_never_renders_the_azdo_pat() {
    for_each_shape::<AzureDevOpsConfig>(|secret| {
        format!("organization_url: \"{PROBE}\"\npat: \"{secret}\"\n")
    });
}

/// Not one of the five sections #5775 names, and covered here for the reason
/// given in `core::config::credential_serialize`: it derived `Serialize` over a
/// credential exactly as they did, and it is the one key in the set on a live
/// code path.
#[test]
fn serialize_never_renders_the_openrouter_key() {
    for_each_shape::<ClassificationConfig>(|secret| {
        format!("openrouter_api_key: \"{secret}\"\nllm_provider: \"{PROBE}\"\n")
    });

    let unset: ClassificationConfig = load(&format!("llm_provider: \"{PROBE}\"\n"));
    let rendered = serde_json::to_string(&unset).expect("json");
    assert!(
        rendered.contains("\"openrouter_api_key\":null"),
        "an unset key must stay visibly unset: {rendered}"
    );
}

/// The composition case: `Config` and `PmConfig` derive `Serialize` and embed
/// every section above, so serializing a whole loaded config is the shape an
/// operator would actually reach for — a diagnostic dump, a manifest, a cache
/// entry. All seven credentials carry distinct values, so a mask applied to one
/// section cannot cover for another.
#[test]
fn serialize_never_renders_any_credential_from_a_whole_config() {
    // Distinct, and deliberately NOT prefixed with the provider's name: the
    // assertion below rejects each credential's first four characters, and a
    // value starting `github-` or `jira-` would match the section's own YAML key
    // in the output and fail against a correct mask.
    const LINEAR: &str = "Q7Zt2Wm4Bx8Lv6Nc1Rd5Ph0Sj";
    const GITHUB: &str = "Zt2Wm4Bx8Lv6Nc1Rd5Ph0SjQ7";
    const JIRA: &str = "2Wm4Bx8Lv6Nc1Rd5Ph0SjQ7Zt";
    const BB_PASSWORD: &str = "Wm4Bx8Lv6Nc1Rd5Ph0SjQ7Zt2";
    const BB_TOKEN: &str = "m4Bx8Lv6Nc1Rd5Ph0SjQ7Zt2W";
    const AZDO: &str = "4Bx8Lv6Nc1Rd5Ph0SjQ7Zt2Wm";
    const OPENROUTER: &str = "Bx8Lv6Nc1Rd5Ph0SjQ7Zt2Wm4";
    const SECRETS: &[&str] = &[
        LINEAR,
        GITHUB,
        JIRA,
        BB_PASSWORD,
        BB_TOKEN,
        AZDO,
        OPENROUTER,
    ];
    let cfg: Config = load(&format!(
        "linear:\n  \
           api_key: \"{LINEAR}\"\n  \
           ticket_regex: \"{PROBE}\"\n\
         github:\n  \
           token: \"{GITHUB}\"\n\
         jira:\n  \
           token: \"{JIRA}\"\n\
         bitbucket:\n  \
           app_password: \"{BB_PASSWORD}\"\n  \
           token: \"{BB_TOKEN}\"\n\
         classification:\n  \
           openrouter_api_key: \"{OPENROUTER}\"\n\
         pm:\n  \
           azure_devops:\n    \
             organization_url: \"https://dev.azure.com/example\"\n    \
             pat: \"{AZDO}\"\n"
    ));

    let rendered = render(&cfg);
    for secret in SECRETS {
        assert!(
            !rendered.contains(secret),
            "a credential reached the serialized config: {secret}"
        );
        let head: String = secret.chars().take(4).collect();
        assert!(
            !rendered.contains(&head),
            "a leading fragment of {secret} survived: {rendered}"
        );
    }
    assert!(
        rendered.contains(PROBE),
        "redaction must not cost the non-secret fields: {rendered}"
    );
}

/// `ClassificationEngineConfig` holds a clone of the same OpenRouter key one
/// struct downstream. It has never derived `Serialize` and must not gain one —
/// this is what fails if someone adds the derive to make a dump compile.
///
/// Rust has no `!Trait` bound, so the assertion is by coherence: the blanket
/// impl below and a real `Serialize` impl for the type can only coexist while
/// the type does NOT implement `Serialize`. Compiling this test is the check;
/// running it asserts nothing.
#[test]
fn the_classification_engine_config_still_refuses_to_serialize() {
    trait AmbiguousIfImpl<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
    impl<T: ?Sized + serde::Serialize> AmbiguousIfImpl<u8> for T {}
    let _ = <ClassificationEngineConfig as AmbiguousIfImpl<_>>::some_item;
}
