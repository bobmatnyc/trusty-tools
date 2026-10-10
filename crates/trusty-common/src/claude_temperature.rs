//! Which Claude models reject a `temperature` sampling parameter (#9318).
//!
//! Why: Claude 5.5 models reject `temperature` ("`temperature` is deprecated
//! for this model"), so every request trusty-common built for one failed. Four
//! request builders set the field: the Bedrock inference adapter, the Anthropic
//! adapter, the `config keys test` auth probe, and the chat Bedrock provider.
//! One predicate here keeps them in agreement.
//! What: [`accepts_temperature`], read with the same model-id parse
//! trusty-review's Bedrock client uses for #9304.
//! Test: the inline `tests` module below; per site,
//! `build_converse_parts_omits_temperature_only_for_claude_5_5`,
//! `build_body_omits_temperature_only_for_claude_5_5`,
//! `probe_request_omits_temperature_only_for_claude_5_5`,
//! `bedrock_stream_omits_temperature_only_for_claude_5_5`.

/// Model families that reject `temperature`, after [`model_family`] has
/// reduced an id to its family.
const NO_TEMPERATURE_FAMILIES: &[&str] =
    &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];

/// Routing prefixes a trusty slug may carry ahead of the provider's own id.
const ROUTING_PREFIXES: &[&str] = &["bedrock/", "anthropic/"];

/// Whether a request to `model` may carry a `temperature` field.
///
/// Why: a 5.5-family model rejects the whole request when `temperature` is
/// present (#9318); every older model honours it.
/// What: `false` only when [`model_family`] reduces `model` to a family in
/// [`NO_TEMPERATURE_FAMILIES`]. The family must match exactly, so an id that
/// only contains `5-5` elsewhere (`claude-sonnet-4-5-20250505`) keeps it.
/// Test: `only_the_claude_5_5_families_reject_temperature`,
/// `unclassifiable_ids_keep_temperature`.
pub(crate) fn accepts_temperature(model: &str) -> bool {
    // #9318: an id that cannot be classified (an application-inference-profile
    // ARN, an unknown vendor) keeps `temperature`, as trusty-review does
    // (#9304). For a model that accepts it, dropping it would silently swap the
    // caller's value for the model default; a wrong `true` fails loudly with
    // the provider's own validation error, which names the field.
    model_family(model).is_none_or(|family| !NO_TEMPERATURE_FAMILIES.contains(&family))
}

/// The model family `model` names, or `None` when the id hides it.
///
/// What: strips one routing prefix (`bedrock/`, `anthropic/`); reads the
/// resource name out of a Bedrock `inference-profile` or `foundation-model`
/// ARN (any other ARN is `None`); drops a `<region>.anthropic.` or
/// `anthropic.` vendor head; then drops trailing `-YYYYMMDD` and `-v<N>[:<N>]`
/// segments. `us.anthropic.claude-opus-5-5-20260901-v1:0` → `claude-opus-5-5`.
/// Test: `only_the_claude_5_5_families_reject_temperature`.
fn model_family(model: &str) -> Option<&str> {
    let model = ROUTING_PREFIXES
        .iter()
        .find_map(|prefix| model.strip_prefix(prefix))
        .unwrap_or(model);
    let model = if model.starts_with("arn:") {
        arn_model_name(model)?
    } else {
        model
    };
    Some(strip_version_suffix(anthropic_model_name(model)))
}

/// The model id inside a Bedrock model ARN whose resource names one.
///
/// Why: an application inference profile's ARN is opaque — its name does not
/// say which model it runs — so it cannot be classified.
/// What: `arn:<partition>:bedrock:<region>:<account>:<kind>/<name>` → `<name>`
/// for `inference-profile` and `foundation-model`; `None` otherwise.
/// Test: `unclassifiable_ids_keep_temperature`.
fn arn_model_name(arn: &str) -> Option<&str> {
    // `<name>` may itself hold `:` (`…-v1:0`), so split at most six ways.
    let resource = arn.splitn(6, ':').nth(5)?;
    match resource.split_once('/')? {
        ("inference-profile" | "foundation-model", name) if !name.is_empty() => Some(name),
        _ => None,
    }
}

/// The name after the `anthropic.` vendor in `<region>.anthropic.<name>` or
/// `anthropic.<name>`; any other id comes back unchanged.
fn anthropic_model_name(model: &str) -> &str {
    const VENDOR: &str = "anthropic.";
    model
        .split_once('.')
        .and_then(|(_, tail)| tail.strip_prefix(VENDOR))
        .or_else(|| model.strip_prefix(VENDOR))
        .unwrap_or(model)
}

/// `model` without trailing `-YYYYMMDD` date and `-v<N>[:<N>]` version
/// segments.
fn strip_version_suffix(model: &str) -> &str {
    let mut end = model.len();
    while let Some(dash) = model[..end].rfind('-') {
        let segment = &model[dash + 1..end];
        let is_date = segment.len() == 8 && segment.bytes().all(|b| b.is_ascii_digit());
        let is_version = segment.len() > 1
            && segment.starts_with('v')
            && segment[1..]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b':');
        if !(is_date || is_version) {
            break;
        }
        end = dash;
    }
    &model[..end]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::accepts_temperature;

    /// Every id form a 5.5-family model arrives in: Anthropic API ids, dated
    /// ids, routing-prefixed slugs, Bedrock profiles, and Bedrock ARNs.
    pub(crate) const CLAUDE_5_5_IDS: &[&str] = &[
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-opus-5-5-20260901",
        "anthropic/claude-sonnet-5-5",
        "anthropic.claude-haiku-5-5",
        "us.anthropic.claude-opus-5-5",
        "global.anthropic.claude-sonnet-5-5",
        "apac.anthropic.claude-haiku-5-5-20260901-v1:0",
        "bedrock/us.anthropic.claude-sonnet-5-5",
        "arn:aws:bedrock:us-east-2:111122223333:inference-profile/us.anthropic.claude-opus-5-5",
        "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-5-5-20260901-v1:0",
    ];

    /// Older models, and ids that merely contain `5-5` outside the family.
    pub(crate) const TEMPERATURE_IDS: &[&str] = &[
        "claude-sonnet-4-5",
        "claude-opus-4-8",
        "claude-sonnet-4-5-20250505",
        "us.anthropic.claude-sonnet-4-6",
        "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        "bedrock/us.anthropic.claude-sonnet-4-5",
        "claude-opus-5-5-preview",
        "claude-opus-5-50",
        "claude-sonnet-5",
        "my-claude-opus-5-5",
        "gpt-4o-mini",
        "openai/gpt-4o-mini",
    ];

    /// Why: the 5.5 families reject `temperature` (#9318); no other model
    /// may lose it.
    /// What: every 5.5 id form is `false`; every older or look-alike id is
    /// `true`.
    /// Test: itself.
    #[test]
    fn only_the_claude_5_5_families_reject_temperature() {
        for model in CLAUDE_5_5_IDS {
            assert!(!accepts_temperature(model), "{model} must omit temperature");
        }
        for model in TEMPERATURE_IDS {
            assert!(accepts_temperature(model), "{model} must keep temperature");
        }
    }

    /// Why: the fail-open side (#9318) — an id that hides its model keeps
    /// `temperature`, matching trusty-review (#9304).
    /// Test: itself.
    #[test]
    fn unclassifiable_ids_keep_temperature() {
        for model in [
            "arn:aws:bedrock:us-east-1:111122223333:application-inference-profile/a1b2c3",
            "arn:aws:bedrock:us-east-1:111122223333:agent/claude-opus-5-5",
            "arn:aws:bedrock:us-east-1",
            "",
        ] {
            assert!(
                accepts_temperature(model),
                "{model:?} must keep temperature"
            );
        }
    }
}
